//! Public native construction/rendering uses one cache and preflight authority.

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use fmn::prelude::*;
use fmn::rendering::{render_bundle_with_fs, render_camera_with_fs};
use fmn::tex::{LineAlign, Mode, Style as MathStyle, TexError};
use fmn::text::TextError;
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_scene::CameraRig;

const FORMULA: &str = r"\frac{x^2+1}{2}";
const PREAMBLE: &str = r"\newcommand{\answer}{x}";
const TEXT: &str = r"Area $x^2$\\Result";

fn cache_root() -> &'static str {
    if cfg!(windows) {
        r"C:\typeset"
    } else {
        "/typeset"
    }
}

fn options(output: &str, workers: usize) -> RenderOptions {
    let mut options = RenderOptions::new(output).unwrap();
    options.config.camera.resolution = (96, 64);
    options.config.camera.fps = 10;
    options.config.sizes.frame_height = 4.0;
    options.config.determinism.mode = DeterminismMode::Certified;
    options.config.render.threads = ThreadPolicy::Fixed(1);
    options.config.directories.cache = cache_root().to_owned();
    options.typeset_preflight_workers = NonZeroUsize::new(workers).unwrap();
    options
}

#[derive(Default)]
struct FormulaScene {
    before_build: u64,
    after_build: u64,
    constructed: bool,
}

impl SceneConstruct for FormulaScene {
    fn tex_preflight(&self) -> Vec<TypesetRequest<'_>> {
        let mut macro_request = TypesetRequest::math(r"\answer^2");
        macro_request.preamble = PREAMBLE;
        let mut text = TypesetRequest::text(TEXT);
        text.align = LineAlign::Right;
        vec![TypesetRequest::math(FORMULA), macro_request, text]
    }

    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        self.constructed = true;
        let engine = stage.tex_engine()?;
        self.before_build = engine.layout_computations();
        let formula = Tex::new(FORMULA).build(engine)?;
        let expanded = Tex::new(r"\answer^2").preamble(PREAMBLE).build(engine)?;
        let text = TexText::new(TEXT)
            .line_align(LineAlign::Right)
            .build(engine)?;
        self.after_build = engine.layout_computations();
        assert_eq!(expanded.typeset.source, r"\answer^2");
        let formula = stage.add(formula)?;
        stage.shift(formula, [-1.5, 0.5, 0.0]);
        let expanded = stage.add(expanded)?;
        stage.shift(expanded, [1.0, 0.5, 0.0]);
        let text = stage.add(text)?;
        stage.shift(text, [-1.0, -1.0, 0.0]);
        Ok(())
    }
}

fn png(fs: &VirtualFs, directory: &str) -> Vec<u8> {
    fs.read(&Path::new(directory).join("frame_000000.png"))
        .unwrap()
}

#[test]
fn cold_warm_and_disabled_cache_render_identical_pngs_without_build_time_layouts() {
    let fs = Arc::new(VirtualFs::new());
    let mut cold_scene = FormulaScene::default();
    let cold = render_with_fs(&mut cold_scene, options("/cold", 1), fs.clone()).unwrap();
    assert!(cold_scene.constructed);
    assert!(cold_scene.before_build > 0);
    assert_eq!(cold_scene.before_build, cold_scene.after_build);
    assert!(cold.typesetting.persistent);
    assert_eq!(cold.artifact.frame_count, 1);

    let mut warm_scene = FormulaScene::default();
    let warm = render_with_fs(&mut warm_scene, options("/warm", 4), fs.clone()).unwrap();
    assert!(warm_scene.constructed);
    assert_eq!(warm_scene.before_build, 0);
    assert_eq!(warm_scene.after_build, 0);
    assert_eq!(warm.typesetting.layout_computations, 0);
    assert!(warm.typesetting.persistent_hits >= 4);
    assert_eq!(png(&fs, "/cold"), png(&fs, "/warm"));

    let memory_fs = Arc::new(VirtualFs::new());
    let mut memory_options = options("/memory", 4);
    memory_options.typeset_cache = false;
    let mut memory_scene = FormulaScene::default();
    let memory = render_with_fs(&mut memory_scene, memory_options, memory_fs.clone()).unwrap();
    assert!(!memory.typesetting.persistent);
    assert_eq!(memory_scene.before_build, memory_scene.after_build);
    assert!(!memory_fs.exists(Path::new(cache_root())));
    assert_eq!(png(&fs, "/cold"), png(&memory_fs, "/memory"));
}

struct ShapeOnly;

#[test]
fn render_completion_evicts_disk_layouts_without_changing_certified_pngs() {
    let fs = Arc::new(VirtualFs::new());
    let mut limited_options = options("/limited", 4);
    limited_options.config.cache.max_bytes = 0;
    let limited =
        render_with_fs(&mut FormulaScene::default(), limited_options, fs.clone()).unwrap();
    assert_eq!(limited.typesetting.persistent_ceiling_bytes, Some(0));
    assert_eq!(limited.typesetting.maintenance.runs, 1);
    assert_eq!(limited.typesetting.maintenance.errors, 0);
    assert!(limited.typesetting.maintenance.evicted_entries >= 4);
    assert!(limited.typesetting.maintenance.evicted_bytes > 0);
    assert_eq!(limited.typesetting.maintenance.retained_bytes, Some(0));

    let after = render_with_fs(
        &mut FormulaScene::default(),
        options("/after", 1),
        fs.clone(),
    )
    .unwrap();
    assert_eq!(after.typesetting.persistent_hits, 0);
    assert!(after.typesetting.layout_computations >= 4);
    let mut memory_options = options("/memory", 4);
    memory_options.typeset_cache = false;
    let memory = render_with_fs(&mut FormulaScene::default(), memory_options, fs.clone()).unwrap();
    assert_eq!(memory.typesetting.maintenance.runs, 0);
    assert_eq!(png(&fs, "/limited"), png(&fs, "/after"));
    assert_eq!(png(&fs, "/limited"), png(&fs, "/memory"));
}

impl SceneConstruct for ShapeOnly {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        stage.add(Circle::new())?;
        Ok(())
    }
}

#[test]
fn shape_only_scenes_do_not_initialize_fonts_or_create_a_typeset_cache() {
    let fs = Arc::new(VirtualFs::new());
    let mut options = options("/shapes", 1);
    options.config.tex.template = "not-needed-by-this-scene".to_owned();
    options.config.text.font = "also-not-needed-by-this-scene".to_owned();
    let report = render_with_fs(&mut ShapeOnly, options, fs.clone()).unwrap();
    assert_eq!(report.typesetting, TypesetSessionReport::default());
    assert!(!fs.exists(Path::new(cache_root())));
}

#[test]
fn an_unowned_cache_root_is_reported_but_does_not_prevent_real_ink() {
    let fs = Arc::new(VirtualFs::new());
    let foreign = Path::new(cache_root()).join("notes.txt");
    fs.insert(&foreign, b"not a cache".to_vec());
    let before = fs.list_dir(Path::new(cache_root())).unwrap();
    let mut scene = FormulaScene::default();
    let report = render_with_fs(&mut scene, options("/fallback", 4), fs.clone()).unwrap();
    assert!(!report.typesetting.persistent);
    assert!(report.typesetting.cache_error.is_some());
    assert_eq!(scene.before_build, scene.after_build);
    assert_eq!(fs.read(&foreign).unwrap(), b"not a cache");
    assert_eq!(fs.list_dir(Path::new(cache_root())).unwrap(), before);
    let image =
        fmn_codec::decode_png(&png(&fs, "/fallback"), &fmn_codec::PngLimits::default()).unwrap();
    assert!(
        image
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[0] > 128)
    );
}

struct BadFormula {
    constructed: bool,
}

impl SceneConstruct for BadFormula {
    fn tex_preflight(&self) -> Vec<TypesetRequest<'_>> {
        vec![
            TypesetRequest::math(r"\fmnUnknownConstruct"),
            TypesetRequest::math("x+1"),
        ]
    }

    fn construct(&mut self, _: &mut Stage<'_>) -> fmn::Result<()> {
        self.constructed = true;
        Ok(())
    }
}

#[test]
fn native_formula_errors_stop_construction_but_later_valid_requests_still_warm() {
    let mut program = BadFormula { constructed: false };
    let session = TexSession::default();
    let result = run_scene_with_typesetting(
        &mut program,
        RuntimeConfig::default(),
        0,
        &mut NullSceneSink,
        &session,
        NonZeroUsize::new(4).unwrap(),
    );
    assert!(matches!(result, Err(Error::TexEngine(TexError::Math(_)))));
    assert!(!program.constructed);
    let engine = session.engine().unwrap();
    let before = engine.layout_computations();
    engine
        .typeset(Mode::Math(MathStyle::Display), "x+1")
        .unwrap();
    assert_eq!(engine.layout_computations(), before);
}

#[test]
fn failed_preflight_does_not_publish_a_partial_artifact() {
    let fs = Arc::new(VirtualFs::new());
    let mut program = BadFormula { constructed: false };
    assert!(render_with_fs(&mut program, options("/failed", 4), fs.clone()).is_err());
    assert!(!program.constructed);
    assert!(!fs.exists(Path::new("/failed")));
}

struct Oversized {
    source: String,
    count: usize,
}

impl SceneConstruct for Oversized {
    fn tex_preflight(&self) -> Vec<TypesetRequest<'_>> {
        vec![TypesetRequest::math(&self.source); self.count]
    }

    fn construct(&mut self, _: &mut Stage<'_>) -> fmn::Result<()> {
        panic!("oversized preflight must fail before construction")
    }
}

#[test]
fn batch_count_source_size_and_total_bytes_are_admitted_before_engine_initialization() {
    for (length, count) in [(1, 4097), (262_145, 1), (262_144, 17)] {
        let mut program = Oversized {
            source: "x".repeat(length),
            count,
        };
        let session = TexSession::default();
        let result = run_scene_with_typesetting(
            &mut program,
            RuntimeConfig::default(),
            0,
            &mut NullSceneSink,
            &session,
            NonZeroUsize::MIN,
        );
        let error = result.err().expect("oversized batch must be refused");
        assert_eq!(error.kind(), ErrorKind::Budget);
        assert!(matches!(
            error,
            Error::TexPreflight(TexPreflightError::Limit { .. })
        ));
        assert_eq!(session.report(), TypesetSessionReport::default());
    }
}

#[test]
fn camera_rendering_uses_the_same_persistent_session_as_affine_rendering() {
    let fs = Arc::new(VirtualFs::new());
    render_with_fs(
        &mut FormulaScene::default(),
        options("/affine", 1),
        fs.clone(),
    )
    .unwrap();
    let report = render_camera_with_fs(
        |scene, camera| {
            let rig = CameraRig::new(scene, camera)?;
            Ok((FormulaScene::default(), rig))
        },
        options("/camera", 4),
        fs,
    )
    .unwrap();
    assert!(report.typesetting.persistent);
    assert_eq!(report.typesetting.layout_computations, 0);
    assert!(report.typesetting.persistent_hits >= 4);
}

#[test]
fn camera_render_completion_obeys_retention_without_changing_pngs() {
    let fs = Arc::new(VirtualFs::new());
    let mut reference = None;
    for (index, threads) in [1, 4, 16].into_iter().enumerate() {
        let destination = format!("/camera-retention-{threads}");
        let mut settings = options(&destination, 4);
        settings.config.render.threads = ThreadPolicy::Fixed(threads);
        settings.typeset_cache = index != 2;
        if index == 0 {
            settings.config.cache.max_bytes = 0;
        }
        let report = render_camera_with_fs(
            |scene, camera| {
                let rig = CameraRig::new(scene, camera)?;
                Ok((FormulaScene::default(), rig))
            },
            settings,
            fs.clone(),
        )
        .unwrap();
        let maintenance = &report.typesetting.maintenance;
        assert_eq!(maintenance.errors, 0);
        if index == 0 {
            assert_eq!(report.typesetting.persistent_ceiling_bytes, Some(0));
            assert_eq!(maintenance.runs, 1);
            assert!(maintenance.evicted_entries >= 4);
            assert_eq!(maintenance.retained_bytes, Some(0));
        } else if index == 1 {
            assert!(report.typesetting.persistent);
            assert_eq!(report.typesetting.persistent_hits, 0);
            assert_eq!(maintenance.runs, 1);
            let retained = maintenance.retained_bytes.unwrap();
            assert!(retained > 0);
            assert!(retained <= report.typesetting.persistent_ceiling_bytes.unwrap());
        } else {
            assert!(!report.typesetting.persistent);
            assert_eq!(maintenance.runs, 0);
        }
        let frame = png(&fs, &destination);
        if let Some(reference) = &reference {
            assert_eq!(reference, &frame);
        } else {
            reference = Some(frame);
        }
    }
}

#[test]
fn memory_scene_runs_expose_observed_work_and_bundle_export_honors_the_template() {
    let mut scene = FormulaScene::default();
    let completed = run_scene(&mut scene, RuntimeConfig::default(), 0, &mut NullSceneSink).unwrap();
    assert!(completed.typesetting_report().initialized);
    assert!(!completed.typesetting_report().persistent);
    assert_eq!(scene.before_build, scene.after_build);

    let mut options = BundleExportOptions::new().unwrap();
    options.config.tex.template = "not-a-native-template".to_owned();
    let mut scene = FormulaScene::default();
    assert!(matches!(
        export_bundle_bytes(&mut scene, options),
        Err(BundleExportError::Scene(Error::TexEngine(TexError::Pack(
            _
        ))))
    ));
    assert!(!scene.constructed);
}

/// A formula built in `construct`, then a held segment of real frames.
struct HeldFormula;

impl SceneConstruct for HeldFormula {
    fn tex_preflight(&self) -> Vec<TypesetRequest<'_>> {
        vec![TypesetRequest::math(FORMULA)]
    }

    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let formula = Tex::new(FORMULA).build(stage.tex_engine()?)?;
        stage.add(formula)?;
        stage.wait(0.3)?;
        Ok(())
    }
}

/// Typesets one queued formula per capture: stand-in for a dynamic string
/// that only exists once frames are running (an updater's readout, say).
struct TypesettingSink<'a> {
    session: &'a TexSession,
    pending: Vec<&'static str>,
    captures: usize,
}

impl SceneSink for TypesettingSink<'_> {
    fn capture(
        &mut self,
        _reason: CaptureReason,
        _packet: FramePacket,
    ) -> Result<(), IntegrationError> {
        self.captures += 1;
        if let Some(source) = self.pending.pop() {
            self.session
                .engine()
                .and_then(|engine| engine.typeset(Mode::Math(MathStyle::Display), source))
                .map_err(|error| IntegrationError::new("test sink", error.to_string()))?;
        }
        Ok(())
    }
}

#[test]
fn the_report_separates_preflighted_layouts_from_typesetting_inside_play() {
    let run = |pending: Vec<&'static str>| {
        let session = TexSession::default();
        let mut sink = TypesettingSink {
            session: &session,
            pending,
            captures: 0,
        };
        let completed = run_scene_with_typesetting(
            &mut HeldFormula,
            RuntimeConfig {
                fps: 10,
                ..RuntimeConfig::default()
            },
            0,
            &mut sink,
            &session,
            NonZeroUsize::new(2).unwrap(),
        )
        .unwrap();
        assert!(sink.captures >= 3, "the held segment produced frames");
        completed.typesetting_report().clone()
    };

    // Everything static was preflighted: the formula and the calibration
    // probe are laid out before the first frame and nothing inside play.
    let clean = run(Vec::new());
    assert_eq!(clean.preflight.requests, 1);
    assert_eq!(clean.layout_computations, 2);
    // Both layouts happened in the preflight, before construct() began.
    assert_eq!(clean.layouts_before_construct, Some(2));
    assert_eq!(clean.layouts_before_first_frame, Some(2));
    assert_eq!(clean.layouts_inside_segments, 0);

    // Planted: two strings typeset while frames run are caught as such.
    let dynamic = run(vec![r"\sqrt{2}", r"\frac{3}{4}"]);
    assert_eq!(dynamic.layouts_before_first_frame, Some(2));
    assert_eq!(dynamic.layouts_inside_segments, 2);
    assert_eq!(dynamic.layout_computations, 4);
}

#[derive(Default)]
struct ConfiguredFontScene {
    explicit_font: Option<&'static str>,
    markup: bool,
    inline_font: bool,
}

impl SceneConstruct for ConfiguredFontScene {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let source = if self.inline_font {
            r#"<span font_family="Computer Modern">Config</span>"#
        } else {
            "Config"
        };
        let mut builder = if self.markup {
            stage.markup_text(source)
        } else {
            stage.text(source)
        };
        if let Some(font) = self.explicit_font {
            builder = builder.font(font);
        }
        let text = builder.font_size(72.0).build(stage.font_book()?)?;
        let text = stage.add(text)?;
        stage.shift(text, [-1.0, 0.0, 0.0]);
        Ok(())
    }
}

fn configured_font_png(
    fs: &Arc<VirtualFs>,
    destination: &str,
    family: &str,
    mut scene: ConfiguredFontScene,
) -> Vec<u8> {
    let mut settings = options(destination, 1);
    settings.config.text.font = family.to_owned();
    render_with_fs(&mut scene, settings, fs.clone()).unwrap();
    png(fs, destination)
}

#[test]
fn configured_native_font_changes_pixels_without_leaking_between_runs() {
    let fs = Arc::new(VirtualFs::new());
    let cm = configured_font_png(
        &fs,
        "/font-cm",
        "Computer Modern",
        ConfiguredFontScene::default(),
    );
    let plex = configured_font_png(
        &fs,
        "/font-plex",
        "IBM Plex Sans",
        ConfiguredFontScene::default(),
    );
    assert_ne!(
        cm, plex,
        "configured families must select different native glyph outlines"
    );
    let repeated = configured_font_png(
        &fs,
        "/font-cm-again",
        "Computer Modern",
        ConfiguredFontScene::default(),
    );
    assert_eq!(
        cm, repeated,
        "a prior scene's font must not become a process default"
    );
    let explicit = configured_font_png(
        &fs,
        "/font-explicit",
        "IBM Plex Sans",
        ConfiguredFontScene {
            explicit_font: Some("Computer Modern"),
            ..Default::default()
        },
    );
    assert_eq!(
        cm, explicit,
        "an authored builder font must override the configured family"
    );
}

#[test]
fn configured_markup_font_keeps_builder_and_inner_span_precedence() {
    let fs = Arc::new(VirtualFs::new());
    let cm = configured_font_png(
        &fs,
        "/markup-reference-cm",
        "Computer Modern",
        ConfiguredFontScene::default(),
    );
    let plex = configured_font_png(
        &fs,
        "/markup-reference-plex",
        "IBM Plex Sans",
        ConfiguredFontScene::default(),
    );
    let inherited = configured_font_png(
        &fs,
        "/markup-inherited",
        "IBM Plex Sans",
        ConfiguredFontScene {
            markup: true,
            ..Default::default()
        },
    );
    assert_eq!(plex, inherited);
    let explicit = configured_font_png(
        &fs,
        "/markup-explicit",
        "IBM Plex Sans",
        ConfiguredFontScene {
            markup: true,
            explicit_font: Some("Computer Modern"),
            ..Default::default()
        },
    );
    assert_eq!(cm, explicit);
    let inner = configured_font_png(
        &fs,
        "/markup-inner",
        "IBM Plex Sans",
        ConfiguredFontScene {
            markup: true,
            inline_font: true,
            ..Default::default()
        },
    );
    assert_eq!(
        cm, inner,
        "authored span font must retain precedence over the inherited default"
    );
}

struct ConfiguredText {
    family: &'static str,
    explicit: Option<&'static str>,
}

impl ConfiguredText {
    fn new(family: &'static str) -> Self {
        Self {
            family,
            explicit: None,
        }
    }
}

impl SceneConstruct for ConfiguredText {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let mut builder = stage.text("AVM wide");
        if let Some(font) = self.explicit {
            builder = builder.font(font);
        }
        let text = builder.build(stage.font_book()?)?;
        assert!(!text.layout.glyphs.is_empty());
        assert!(
            text.layout
                .glyphs
                .iter()
                .all(|glyph| glyph.face.family == self.family),
            "the source scene must observe the configured or explicitly authored face"
        );
        stage.add(text)?;
        Ok(())
    }
}

#[test]
fn unavailable_configured_font_preserves_the_named_refusal_and_publishes_nothing() {
    let fs = Arc::new(VirtualFs::new());
    let mut settings = options("/unavailable-font", 1);
    settings.config.text.font = "Unbundled Lesson Font".to_owned();
    let error = render_with_fs(
        &mut ConfiguredText::new("Unbundled Lesson Font"),
        settings,
        fs.clone(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RenderError::Scene(Error::Text(fmn::library::TextMobjectError::Text(
            TextError::FontUnavailable { ref family, .. }
        ))) if family == "Unbundled Lesson Font"
    ));
    assert!(!fs.exists(Path::new("/unavailable-font")));

    // An authored available face does not require the unused default to exist.
    let mut settings = options("/explicit-available-font", 1);
    settings.config.text.font = "Unbundled Lesson Font".to_owned();
    let mut scene = ConfiguredText {
        family: "CM Typewriter",
        explicit: Some("CM Typewriter"),
    };
    render_with_fs(&mut scene, settings, fs).unwrap();
}

#[test]
fn configured_font_reaches_camera_svg_and_recorded_scene_construction() {
    let fs = Arc::new(VirtualFs::new());
    let mut direct = options("/configured-direct", 1);
    direct.config.text.font = "IBM Plex Sans".to_owned();
    let config = direct.config.clone();
    render_with_fs(
        &mut ConfiguredText::new("IBM Plex Sans"),
        direct,
        fs.clone(),
    )
    .unwrap();

    let mut export = BundleExportOptions::new().unwrap();
    export.config = config.clone();
    let bundle = export_bundle_bytes(&mut ConfiguredText::new("IBM Plex Sans"), export).unwrap();
    let mut replay = options("/configured-replay", 1);
    replay.config = config.clone();
    render_bundle_with_fs(&bundle.bundle.bytes, replay, fs.clone()).unwrap();
    assert_eq!(
        png(&fs, "/configured-direct"),
        png(&fs, "/configured-replay")
    );

    let mut camera = options("/configured-camera", 1);
    camera.config = config.clone();
    render_camera_with_fs(
        |scene, camera| {
            Ok((
                ConfiguredText::new("IBM Plex Sans"),
                CameraRig::new(scene, camera)?,
            ))
        },
        camera,
        fs.clone(),
    )
    .unwrap();
    assert!(!png(&fs, "/configured-camera").is_empty());

    let mut svg = SvgRenderOptions::new("/configured.svg").unwrap();
    svg.config = config;
    render_svg_with_fs(&mut ConfiguredText::new("IBM Plex Sans"), svg, fs.clone()).unwrap();
    let source = String::from_utf8(fs.read(Path::new("/configured.svg")).unwrap()).unwrap();
    assert!(source.contains("<path"));
}
