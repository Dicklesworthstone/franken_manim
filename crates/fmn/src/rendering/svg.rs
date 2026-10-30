//! Final-state vector publication through the ordinary native scene lifecycle.

use std::fmt;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use fmn_anim::FramePacket;
use fmn_config::Config;
use fmn_core::color::{HexParseError, Srgb};
use fmn_geom::GeomError;
use fmn_library::svg_export::stage_svg_document_with_background;
use fmn_output::{SinkAdapterError, SvgPublicationConfig, SvgPublicationReport, publish_svg_new};
use fmn_platform::clock::StdClock;
use fmn_platform::fs::{FileSystem, FsError, StdFs};
use fmn_render::svg::{SvgLimits, SvgProjectionError, project_svg_stage};
use fmn_render::{Camera, CameraConfig, CameraError, CameraFrame};
use fmn_scene::{
    CameraRig, CaptureReason, IntegrationError, LifecycleEvent, RuntimeConfig, Scene,
    SceneRunReport, SceneSink,
};
use fmn_tex::{TexSession, TypesetSessionReport};

use crate::{ProgramAdapter, SceneConstruct};

/// Native scene-to-SVG configuration. No pixel pipeline or encoder is created.
#[derive(Clone, Debug)]
pub struct SvgRenderOptions {
    /// New file destination; existing files, directories and links are refused.
    pub output: PathBuf,
    /// The native scene clock, seed, typesetting and default camera settings.
    pub config: Config,
    /// Optional explicit camera pose and background. Its resolution and FPS
    /// must match the scene configuration. Otherwise use [`Self::camera_config`].
    pub camera: Option<CameraConfig>,
    /// Maximum vector geometry, counting repeated draw placements.
    pub geometry: SvgLimits,
    /// Maximum scene time in frame ticks and maximum capture requests.
    /// Updaters run normally even though only the completed scene is exported.
    pub max_scene_frames: u64,
    /// Maximum complete SVG document size.
    pub max_output_bytes: u64,
    /// Use the configured persistent native typesetting cache.
    pub typeset_cache: bool,
    /// Ceiling for declared native typesetting preflight workers.
    pub typeset_preflight_workers: NonZeroUsize,
}

impl SvgRenderOptions {
    /// Bundled configuration and bounded final-state vector publication.
    ///
    /// # Errors
    /// Returns the existing error if bundled configuration cannot be resolved.
    pub fn new(output: impl Into<PathBuf>) -> Result<Self, fmn_config::ConfigError> {
        Ok(Self {
            output: output.into(),
            config: Config::resolve(&[], None)?.config,
            camera: None,
            geometry: SvgLimits::default(),
            max_scene_frames: 1_000_000,
            max_output_bytes: 64 * 1024 * 1024,
            typeset_cache: true,
            typeset_preflight_workers: crate::typesetting::DEFAULT_PREFLIGHT_WORKERS,
        })
    }

    /// Build the same configured viewport, background and default camera as
    /// a native picture. An explicit camera may then move, zoom or rotate it.
    ///
    /// # Errors
    /// Invalid viewport, frame geometry or background color.
    pub fn camera_config(&self) -> Result<CameraConfig, SvgRenderError> {
        let (width, height) = self.config.camera.resolution;
        if width == 0 || height == 0 || width > 16384 || height > 16384 {
            return Err(SvgRenderError::InvalidOptions(
                "SVG requires pixel dimensions in 1..=16384",
            ));
        }
        let frame_height = self.config.sizes.frame_height;
        let mut frame = CameraFrame::default();
        frame
            .set_shape([
                frame_height * f64::from(width) / f64::from(height),
                frame_height,
            ])
            .map_err(SvgRenderError::Camera)?;
        Ok(CameraConfig {
            resolution: (width, height),
            fps: RuntimeConfig::from_config(&self.config).effective_fps(),
            background: Srgb::from_hex(&self.config.camera.background_color)
                .map_err(SvgRenderError::Color)?
                .to_linear(self.config.camera.background_opacity),
            frame,
            ..CameraConfig::default()
        })
    }

    fn admit(&self, fs: &dyn FileSystem) -> Result<CameraConfig, SvgRenderError> {
        if self.output.as_os_str().is_empty() || self.output.file_name().is_none() {
            return Err(SvgRenderError::InvalidOptions(
                "SVG output must name a file",
            ));
        }
        if self.max_scene_frames == 0
            || self.max_output_bytes == 0
            || self.geometry.max_points == 0
            || self.geometry.max_shapes == 0
        {
            return Err(SvgRenderError::InvalidOptions(
                "SVG budgets must be nonzero",
            ));
        }
        let configured = self.camera_config()?;
        let camera = self.camera.clone().unwrap_or_else(|| configured.clone());
        if camera.resolution != configured.resolution || camera.fps != configured.fps {
            return Err(SvgRenderError::InvalidOptions(
                "SVG camera resolution and FPS must match the scene configuration",
            ));
        }
        Camera::new(camera.clone()).map_err(SvgRenderError::Camera)?;
        if fs
            .node_kind_no_follow(&self.output)
            .map_err(SvgRenderError::FileSystem)?
            .is_some()
        {
            return Err(SvgRenderError::InvalidOptions(
                "SVG destination already exists",
            ));
        }
        Ok(camera)
    }

    fn typesetting(&self, fs: Arc<dyn FileSystem>) -> TexSession {
        if self.typeset_cache {
            TexSession::with_cache(&self.config, fs, Arc::new(StdClock::new()))
        } else {
            TexSession::memory(self.config.tex.template.clone())
        }
    }
}

/// Completed SVG and actual native scene/typesetting work. The artifact is a
/// vector document, not a raster frame or a certified input-closure manifest.
#[derive(Clone, Debug)]
pub struct SvgRenderReport {
    pub scene: SceneRunReport,
    pub artifact: SvgPublicationReport,
    pub typesetting: TypesetSessionReport,
}

/// Typed SVG admission, native execution, projection or publication refusal.
#[derive(Debug)]
pub enum SvgRenderError {
    InvalidOptions(&'static str),
    Scene(crate::Error),
    Camera(CameraError),
    Color(HexParseError),
    Projection(SvgProjectionError),
    Geometry(GeomError),
    FileSystem(FsError),
    Publication(SinkAdapterError),
}

impl fmt::Display for SvgRenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOptions(message) => f.write_str(message),
            Self::Scene(error) => error.fmt(f),
            Self::Camera(error) => error.fmt(f),
            Self::Color(error) => error.fmt(f),
            Self::Projection(error) => error.fmt(f),
            Self::Geometry(error) => error.fmt(f),
            Self::FileSystem(error) => error.fmt(f),
            Self::Publication(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for SvgRenderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidOptions(_) => None,
            Self::Scene(error) => Some(error),
            Self::Camera(error) => Some(error),
            Self::Color(error) => Some(error),
            Self::Projection(error) => Some(error),
            Self::Geometry(error) => Some(error),
            Self::FileSystem(error) => Some(error),
            Self::Publication(error) => Some(error),
        }
    }
}

/// Run an ordinary native scene and publish its final vector state as SVG.
///
/// The existing clock executes animations, updaters and teardown normally.
/// Only the completed state is projected. Camera geometry, fixed overlays,
/// background and zoom-dependent strokes use Lumen's shared vector admission.
/// Nonrepresentable images, surfaces, gradients and depth/perspective effects
/// fail before output creation. Chisel's ordinary SVG paint/compositing contract
/// applies; browser antialiasing is not Lumen's raster coverage.
///
/// # Errors
/// Invalid options fail before scene execution. Scene, projection, geometry,
/// budget and create-only publication failures retain their typed sources.
pub fn render_svg<P: SceneConstruct + ?Sized>(
    program: &mut P,
    options: SvgRenderOptions,
) -> Result<SvgRenderReport, SvgRenderError> {
    render_svg_with_fs(program, freeze_host_output(options)?, Arc::new(StdFs))
}

/// [`render_svg`] with an explicit filesystem capability.
///
/// # Errors
/// Returns the same typed errors as [`render_svg`].
pub fn render_svg_with_fs<P: SceneConstruct + ?Sized>(
    program: &mut P,
    options: SvgRenderOptions,
    fs: Arc<dyn FileSystem>,
) -> Result<SvgRenderReport, SvgRenderError> {
    let camera = Camera::new(options.admit(fs.as_ref())?).map_err(SvgRenderError::Camera)?;
    let typesetting = options.typesetting(Arc::clone(&fs));
    let mut sink = SvgCapture::new(options.max_scene_frames);
    let completed = crate::run_scene_with_typography(
        program,
        RuntimeConfig::from_config(&options.config),
        options.config.determinism.seed,
        &mut sink,
        &typesetting,
        &options.config.text,
        options.typeset_preflight_workers,
    )
    .map_err(|error| sink.scene_error(error))?;
    sink.check()?;
    publish(
        completed.scene(),
        *completed.report(),
        completed.typesetting_report().clone(),
        &camera,
        options,
        fs.as_ref(),
    )
}

/// Export a final SVG with an animated native [`CameraRig`]. The factory
/// allocates the rig in the actual scene and returns its ordinary program.
/// It must not play/wait before returning. The final camera is sampled after
/// teardown from the same Stage whose geometry is exported.
///
/// # Errors
/// The same failures as [`render_svg`], plus invalid factory timing or rig
/// handles/values. No output is published for a failed scene or camera.
pub fn render_svg_camera<F, P>(
    factory: F,
    options: SvgRenderOptions,
) -> Result<SvgRenderReport, SvgRenderError>
where
    F: FnOnce(&mut Scene, &CameraConfig) -> crate::Result<(P, CameraRig)>,
    P: SceneConstruct,
{
    render_svg_camera_with_fs(factory, freeze_host_output(options)?, Arc::new(StdFs))
}

fn freeze_host_output(mut options: SvgRenderOptions) -> Result<SvgRenderOptions, SvgRenderError> {
    // Scene/factory callbacks may change the process cwd. Publication must
    // target the same host path that passed admission before they ran.
    // Explicit filesystem capabilities retain their caller-defined semantics.
    options.output = std::path::absolute(&options.output).map_err(|err| {
        SvgRenderError::FileSystem(FsError::Io {
            path: options.output.clone(),
            err,
        })
    })?;
    Ok(options)
}

/// [`render_svg_camera`] with an explicit filesystem capability.
///
/// # Errors
/// Returns the same typed errors as [`render_svg_camera`].
pub fn render_svg_camera_with_fs<F, P>(
    factory: F,
    options: SvgRenderOptions,
    fs: Arc<dyn FileSystem>,
) -> Result<SvgRenderReport, SvgRenderError>
where
    F: FnOnce(&mut Scene, &CameraConfig) -> crate::Result<(P, CameraRig)>,
    P: SceneConstruct,
{
    let base = options.admit(fs.as_ref())?;
    let mut scene = Scene::new(
        RuntimeConfig::from_config(&options.config),
        options.config.determinism.seed,
    )
    .map_err(|error| SvgRenderError::Scene(error.into()))?;
    let (mut program, rig) = factory(&mut scene, &base).map_err(SvgRenderError::Scene)?;
    if scene.time().frames() != 0 || scene.play_count() != 0 {
        return Err(SvgRenderError::InvalidOptions(
            "camera factory must not run playback before returning its program",
        ));
    }
    rig.sample(scene.stage(), &base)
        .map_err(|error| SvgRenderError::Scene(error.into()))?;
    let typesetting = options.typesetting(Arc::clone(&fs));
    let mut sink = SvgCapture::new(options.max_scene_frames);
    let mut adapter = ProgramAdapter {
        program: &mut program,
        front_door_error: None,
        typesetting: &typesetting,
        text_defaults: Some(&options.config.text),
        preflight_workers: options.typeset_preflight_workers,
    };
    let run = scene.run(
        &mut adapter,
        &mut crate::SegmentAccountingSink {
            inner: &mut sink,
            typesetting: &typesetting,
        },
    );
    typesetting.maintain_cache();
    if let Some(error) = adapter.front_door_error.take() {
        return Err(sink.scene_error(error));
    }
    let report = run.map_err(|error| sink.scene_error(error.into()))?;
    sink.check()?;
    let camera = Camera::new(
        rig.sample(scene.stage(), &base)
            .map_err(|error| SvgRenderError::Scene(error.into()))?,
    )
    .map_err(SvgRenderError::Camera)?;
    publish(
        &scene,
        report,
        typesetting.report(),
        &camera,
        options,
        fs.as_ref(),
    )
}

fn publish(
    scene: &Scene,
    report: SceneRunReport,
    typesetting: TypesetSessionReport,
    camera: &Camera,
    options: SvgRenderOptions,
    fs: &dyn FileSystem,
) -> Result<SvgRenderReport, SvgRenderError> {
    let projected = project_svg_stage(scene.stage(), camera, options.geometry)
        .map_err(SvgRenderError::Projection)?;
    let (width, height) = camera.pixel_shape();
    let document = stage_svg_document_with_background(
        &projected,
        f64::from(width),
        f64::from(height),
        f64::from(width),
        f64::from(height),
        Some(camera.background()),
    )
    .map_err(SvgRenderError::Geometry)?;
    let artifact = publish_svg_new(
        fs,
        &SvgPublicationConfig {
            destination: options.output,
            max_artifact_bytes: options.max_output_bytes,
            profile: None,
        },
        document.as_bytes(),
    )
    .map_err(SvgRenderError::Publication)?;
    Ok(SvgRenderReport {
        scene: report,
        artifact,
        typesetting,
    })
}

/// Count native captures without materializing/rasterizing intermediate frames.
struct SvgCapture {
    limit: u64,
    captures: u64,
    failed: bool,
}

impl SvgCapture {
    fn new(limit: u64) -> Self {
        Self {
            limit,
            captures: 0,
            failed: false,
        }
    }

    fn check(&self) -> Result<(), SvgRenderError> {
        if self.failed {
            Err(SvgRenderError::InvalidOptions(
                "SVG scene-frame limit exceeded",
            ))
        } else {
            Ok(())
        }
    }

    fn checkpoint(&mut self, within_limit: bool) -> Result<(), IntegrationError> {
        self.failed |= !within_limit;
        self.check()
            .map_err(|error| IntegrationError::new("native-svg", error.to_string()))
    }

    fn capture_count(&mut self, count: u64) -> Result<(), IntegrationError> {
        let next = self
            .captures
            .checked_add(count)
            .filter(|n| *n <= self.limit);
        self.checkpoint(next.is_some())?;
        self.captures = next.expect("count passed the capture bound");
        Ok(())
    }

    fn scene_error(&self, error: crate::Error) -> SvgRenderError {
        if matches!(&error, crate::Error::Scene(fmn_scene::SceneError::Integration(integration))
            if integration.point() == "native-svg")
            && self.failed
        {
            SvgRenderError::InvalidOptions("SVG scene-frame limit exceeded")
        } else {
            SvgRenderError::Scene(error)
        }
    }
}

impl SceneSink for SvgCapture {
    fn capture_state_required(&self) -> bool {
        false
    }

    fn capture_metadata(
        &mut self,
        _: CaptureReason,
        _: fmn_anim::FrameMetadata,
    ) -> Result<(), IntegrationError> {
        self.capture_count(1)
    }

    fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
        self.checkpoint(u64::try_from(event.time.frames()).is_ok_and(|n| n <= self.limit))
    }

    fn capture(&mut self, _: CaptureReason, _: FramePacket) -> Result<(), IntegrationError> {
        self.capture_count(1)
    }

    fn accepts_pure_segments(&self) -> bool {
        !self.failed
    }

    fn capture_pure_segment(
        &mut self,
        segment: &fmn_anim::pure_frame::PureSegment,
    ) -> Result<(), IntegrationError> {
        let count = u64::try_from(segment.frame_count()).map_err(|_| {
            self.failed = true;
            IntegrationError::new("native-svg", "negative pure-segment frame count")
        })?;
        self.capture_count(count)
    }
}
