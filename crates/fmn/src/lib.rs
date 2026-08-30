//! FrankenManim's first-class Rust API.
//!
//! This crate is the public composition root for native Rust scenes. It keeps
//! the subsystem crates available under named modules, while [`prelude`]
//! exposes the small set of names normally needed to write a scene.
//!
//! The facade delegates construction and playback to Proscenium, Choreo, and
//! Marionette. It does not carry a second scene loop or a simplified animation
//! engine.
//!
//! ```
//! use fmn::prelude::*;
//!
//! #[derive(Default)]
//! struct CircleShift;
//!
//! impl SceneConstruct for CircleShift {
//!     fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
//!         let circle = stage.add(Circle::new().radius(0.8).color(BLUE))?;
//!         let movement = circle
//!             .animate()
//!             .set_anim_args(AnimateArgs {
//!                 run_time: Some(0.1),
//!                 ..AnimateArgs::default()
//!             })?
//!             .shift(RIGHT)?;
//!         stage.play(movement)?;
//!         Ok(())
//!     }
//! }
//!
//! let mut sink = NullSceneSink;
//! let completed = run_scene(
//!     &mut CircleShift,
//!     RuntimeConfig::default(),
//!     7,
//!     &mut sink,
//! )?;
//! assert_eq!(completed.report().play_count, 1);
//! # Ok::<(), fmn::Error>(())
//! ```
// Keep the README's native example in the ordinary rustdoc test gate.
#![doc = include_str!("../../../README.md")]
#![forbid(unsafe_code)]

use std::fmt;

use fmn_anim::{
    AnimError, Animation, IntoAnimation, IntoAnimations, SegmentReport, prepare_animation,
    prepare_animations,
};
use fmn_config::ConfigError;
use fmn_geom::{GeomError, SpaceOpsError};
use fmn_library::{DashError, TexMobjectError, TextMobjectError};
use fmn_mobject::{AnimateError, Mob, Mobject, Stage as MobjectStage, StageError};
use fmn_platform::fetch::FetchError;
use fmn_platform::fs::FsError;
use fmn_platform::process::{FfmpegLocatorError, ProcessError};
use fmn_platform::topology::TopologyError;
use fmn_scene::{
    IntegrationError, PlayOverrides, RuntimeConfig, Scene, SceneError, SceneProgram,
    SceneRunReport, SceneSink,
};
use fmn_tex::TexError;

pub mod prelude;

/// Built-in native scenes shipped with the standalone binary.
///
/// The primitive corpus is both a useful installation smoke test and G1's
/// public, permissively licensed end-to-end corpus. Keeping its scene
/// definitions here means the CLI and the certified conformance target execute
/// exactly the same native programs.
pub mod builtins {
    use crate::prelude::*;

    /// Stable names in the public G1 primitive corpus.
    pub const PRIMITIVE_SCENE_NAMES: [&str; 25] = [
        "circle_shift.v1",
        "rectangle_shift.v1",
        "triangle_shift.v1",
        "pentagon_shift.v1",
        "arc_shift.v1",
        "dot_shift.v1",
        "ellipse_shift.v1",
        "annulus_shift.v1",
        "line_shift.v1",
        "dashed_line_shift.v1",
        "arrow_shift.v1",
        "circle_scale.v1",
        "rectangle_scale.v1",
        "triangle_scale.v1",
        "hexagon_scale.v1",
        "arc_scale.v1",
        "dot_scale.v1",
        "ellipse_scale.v1",
        "annulus_scale.v1",
        "line_scale.v1",
        "dashed_line_scale.v1",
        "arrow_scale.v1",
        "rounded_rectangle.v1",
        "arc_between_points.v1",
        "layered_polygon.v1",
    ];

    const COLORS: [Srgb; 7] = [BLUE_C, GREEN_B, MAROON_C, RED_C, TEAL_B, YELLOW_C, WHITE];

    /// One native program from [`PRIMITIVE_SCENE_NAMES`].
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct PrimitiveScene {
        index: usize,
        name: &'static str,
    }

    impl SceneConstruct for PrimitiveScene {
        fn name(&self) -> &str {
            self.name
        }

        fn construct(&mut self, stage: &mut Stage<'_>) -> crate::Result<()> {
            let color = COLORS[self.index % COLORS.len()];
            let main = stage.add(primitive(self.index, color)?)?;
            stage.set_fill(
                main,
                Some(color),
                Some(0.28 + 0.08 * (self.index % 6) as f64),
                Some(0.0),
                true,
            );
            stage.set_stroke(
                main,
                Some(WHITE),
                Some(1.2 + 0.25 * (self.index % 5) as f64),
                Some(0.95),
                None,
                true,
            );

            // A quiet back layer makes painter ordering and alpha composition
            // part of every sequence, not only the layered fixture.
            let back = stage.add(
                Circle::new()
                    .radius(0.32 + 0.015 * self.index as f64)
                    .arc_center([-0.55, 0.28, 0.0]),
            )?;
            stage.set_fill(
                back,
                Some(COLORS[(self.index + 3) % COLORS.len()]),
                Some(0.32),
                Some(0.0),
                true,
            );
            stage.set_stroke(back, None, Some(0.0), Some(0.0), None, true);
            stage.set_z_index(back, -1, true);
            stage.arena_mut().add_to_scene(back)?;

            let start_x = -0.72 + 0.06 * (self.index % 5) as f64;
            let start_y = -0.32 + 0.11 * (self.index % 7) as f64;
            stage.shift(main, [start_x, start_y, 0.0]);

            let builder = main.animate().set_anim_args(AnimateArgs {
                run_time: Some(0.25),
                rate_func: Some(crate::core::rate::linear),
                ..AnimateArgs::default()
            })?;
            let builder = if self.index < 11 {
                builder.shift([
                    1.0 + 0.04 * (self.index % 4) as f64,
                    0.18 - 0.03 * (self.index % 3) as f64,
                    0.0,
                ])
            } else if self.index < 22 {
                builder.scale(0.82 + 0.035 * (self.index % 6) as f64)
            } else {
                builder
                    .shift([0.76, 0.16, 0.0])
                    .and_then(|builder| builder.scale(0.9))
            }?;
            stage.play(builder)?;
            stage.wait(0.125)?;
            Ok(())
        }
    }

    /// Resolve one built-in primitive scene by its stable name.
    #[must_use]
    pub fn primitive_scene(name: &str) -> Option<PrimitiveScene> {
        PRIMITIVE_SCENE_NAMES
            .iter()
            .position(|candidate| *candidate == name)
            .map(|index| PrimitiveScene {
                index,
                name: PRIMITIVE_SCENE_NAMES[index],
            })
    }
    /// Stable name of the sound-cue scene outside the pinned G1 corpus.
    ///
    /// The 25-name primitive corpus is digest-pinned by the conformance
    /// scene-runtime goldens, so the audio corpus rides beside it: this scene
    /// is selected by explicit name (never `--write_all`) and exercises the
    /// `Scene.add_sound` request boundary end to end.
    pub const SOUND_CUE_SCENE_NAME: &str = "sound_cue.v1";

    /// The sound-cue scene's asset, resolved by the composition root.
    ///
    /// The path is relative: the caller resolves it against the process
    /// working directory (a checked-in deterministic copy ships under
    /// `crates/fmn/assets/`).
    pub const SOUND_CUE_ASSET_NAME: &str = "sound_cue_tone.v1.wav";

    /// The native sound-cue scene: one shape, one `add_sound` request.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct SoundCueScene {
        name: &'static str,
    }

    impl SceneConstruct for SoundCueScene {
        fn name(&self) -> &str {
            self.name
        }

        fn construct(&mut self, stage: &mut Stage<'_>) -> crate::Result<()> {
            let main = stage.add(Circle::new().radius(0.6).color(WHITE))?;
            stage.set_fill(main, Some(TEAL_B), Some(0.3), Some(0.0), true);
            stage.set_stroke(main, Some(WHITE), Some(1.2), Some(0.95), None, true);
            stage.wait(0.5)?;
            stage
                .scene_mut()
                .add_sound(SOUND_CUE_ASSET_NAME, 0.0, None, None)
                .map(|_| ())?;
            stage.wait(0.5)?;
            Ok(())
        }
    }

    /// Resolve the built-in sound-cue scene by its stable name.
    #[must_use]
    pub fn sound_scene(name: &str) -> Option<SoundCueScene> {
        (name == SOUND_CUE_SCENE_NAME).then_some(SoundCueScene {
            name: SOUND_CUE_SCENE_NAME,
        })
    }

    fn primitive(index: usize, color: Srgb) -> crate::Result<Mobject> {
        Ok(match index {
            0 | 11 => Circle::new()
                .radius(0.48 + 0.02 * (index % 4) as f64)
                .color(color)
                .into(),
            1 | 12 => Rectangle::new()
                .width(1.1)
                .height(0.62 + 0.03 * (index % 3) as f64)
                .color(color)
                .build()?
                .into(),
            2 | 13 => Mobject::try_from(RegularPolygon::triangle().radius(0.6).color(color))?,
            3 => Mobject::try_from(RegularPolygon::new(5).radius(0.56).color(color))?,
            14 => Mobject::try_from(RegularPolygon::new(6).radius(0.56).color(color))?,
            4 | 15 => Arc::new()
                .start_angle(-0.4)
                .angle(4.2)
                .radius(0.58)
                .color(color)
                .build()?
                .into(),
            5 | 16 => Dot::new().radius(0.23).color(color).into(),
            6 | 17 => Ellipse::new().width(1.15).height(0.58).color(color).into(),
            7 | 18 => Annulus::new()
                .inner_radius(0.22)
                .outer_radius(0.52)
                .color(color)
                .into(),
            8 | 19 => Line::new([-0.58, -0.25, 0.0], [0.58, 0.25, 0.0])
                .path_arc(0.18)
                .color(color)
                .build()?
                .into(),
            9 | 20 => DashedLine::new([-0.62, 0.0, 0.0], [0.62, 0.0, 0.0])
                .dash_length(0.16)
                .positive_space_ratio(0.55)
                .color(color)
                .build()?
                .into(),
            10 | 21 => Arrow::new([-0.58, 0.0, 0.0], [0.58, 0.0, 0.0])
                .buff(0.0)
                .color(color)
                .build()?
                .into(),
            22 => Rectangle::new()
                .width(1.1)
                .height(0.66)
                .corner_radius(0.16)
                .color(color)
                .build()?
                .into(),
            23 => ArcBetweenPoints::new([-0.58, -0.18, 0.0], [0.58, 0.18, 0.0])
                .angle(1.2)
                .color(color)
                .build()?
                .into(),
            // The only remaining corpus index is 24, the layered polygon.
            _ => Mobject::try_from(RegularPolygon::new(7).radius(0.56).color(color))?,
        })
    }
}

/// Substrate constants, colors, rates, deterministic RNG, and value types.
pub mod core {
    pub use fmn_core::*;
}

/// Typed configuration and preamble-pack selection.
pub mod config {
    pub use fmn_config::*;
}

/// Host capability traits and typed capability failures.
pub mod platform {
    pub use fmn_platform::*;
}

/// Chisel geometry types and operations.
pub mod geometry {
    pub use fmn_geom::*;
}

/// Marionette's arena, handles, record buffers, and fluent builders.
pub mod mobject {
    pub use fmn_mobject::*;
}

/// Choreo animations, clocks, and timeline machinery.
pub mod animation {
    pub use fmn_anim::*;
}

/// Scribe text layout and bundled font book.
pub mod text {
    pub use fmn_text::*;
}

/// Scribe mathematics typesetting.
pub mod tex {
    pub use fmn_tex::*;
}

/// Menagerie and Atlas mobjects.
pub mod library {
    pub use fmn_library::*;
}

/// Proscenium scene runtime and event surface.
pub mod scene {
    pub use fmn_scene::*;
}

/// Top-level result type for native Rust scenes.
pub type Result<T> = std::result::Result<T, Error>;

/// Stable front-door error category.
///
/// The numeric values deliberately match the corresponding generated CLI
/// exit-code rows. `usage`, `cancelled`, and `internal` are CLI/host concerns,
/// so native scene construction does not manufacture them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrorKind {
    /// Invalid configuration (`fmn` exit 3).
    Config = 3,
    /// A required host capability is unavailable (`fmn` exit 4).
    Capability = 4,
    /// Scene construction or execution failed (`fmn` exit 5).
    Scene = 5,
    /// Host I/O, process execution, or publication failed (`fmn` exit 6).
    Render = 6,
    /// A declared resource budget was exhausted (`fmn` exit 8).
    Budget = 8,
}

impl ErrorKind {
    /// Stable schema spelling shared with the CLI.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Capability => "capability",
            Self::Scene => "scene",
            Self::Render => "render",
            Self::Budget => "budget",
        }
    }

    /// Numeric process status shared with the CLI.
    #[must_use]
    pub const fn code(self) -> u8 {
        self as u8
    }
}

/// Errors crossing the native Rust front door.
#[derive(Debug)]
pub enum Error {
    /// Configuration parsing or typed extraction.
    Config(ConfigError),
    /// A geometry constructor or operation.
    Geometry(GeomError),
    /// A bounded public space-ops construction.
    SpaceOps(SpaceOpsError),
    /// A dashed-path construction.
    Dash(DashError),
    /// Arena ownership, family, or record semantics.
    Stage(StageError),
    /// Animation preparation or playback.
    Animation(AnimError),
    /// Scene lifecycle or integration.
    Scene(SceneError),
    /// Native text construction.
    Text(TextMobjectError),
    /// Native math-mobject construction.
    Typesetting(TexMobjectError),
    /// Math-engine initialization or layout.
    TexEngine(TexError),
    /// Filesystem capability I/O.
    FileSystem(FsError),
    /// Host-provided asset fetch.
    AssetFetch(FetchError),
    /// The ffmpeg-only executable locator.
    FfmpegLocator(FfmpegLocatorError),
    /// The ffmpeg-only process boundary.
    Process(ProcessError),
    /// Host topology discovery.
    Topology(TopologyError),
}

impl Error {
    /// Stable category used by robot integrations and process adapters.
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        match self {
            Self::Config(_) => ErrorKind::Config,
            Self::Geometry(
                GeomError::SmoothingSizeOverflow { .. }
                | GeomError::ClosedSmoothingBudgetExceeded { .. }
                | GeomError::SubdivisionBudgetExceeded { .. }
                | GeomError::ToleranceUnreachable { .. }
                | GeomError::ArcComponentOverflow { .. }
                | GeomError::ArcComponentsAboveBudget { .. },
            )
            | Self::SpaceOps(_)
            | Self::Dash(DashError::DashCountOverflow | DashError::TooManyDashes { .. })
            | Self::Stage(StageError::SubmobjectBudgetExceeded { .. })
            | Self::Text(
                TextMobjectError::ResourceLimit { .. }
                | TextMobjectError::CapacityOverflow { .. }
                | TextMobjectError::AllocationFailed { .. },
            )
            | Self::FileSystem(FsError::TooLarge { .. } | FsError::TooManyEntries { .. })
            | Self::AssetFetch(FetchError::TooLarge { .. })
            | Self::FfmpegLocator(
                FfmpegLocatorError::SearchPathLimit { .. }
                | FfmpegLocatorError::ExecutableSizeLimit { .. },
            )
            | Self::Process(
                ProcessError::StdinChunkLimit { .. } | ProcessError::StdinTotalLimit { .. },
            ) => ErrorKind::Budget,
            Self::AssetFetch(_)
            | Self::FfmpegLocator(_)
            | Self::Process(ProcessError::CapabilityAbsent { .. })
            | Self::Topology(_) => ErrorKind::Capability,
            Self::Geometry(_)
            | Self::Dash(_)
            | Self::Stage(_)
            | Self::Animation(_)
            | Self::Scene(
                SceneError::InvalidConfig(_)
                | SceneError::InvalidLifecycle(_)
                | SceneError::InvalidState(_)
                | SceneError::UnboundUpdaters
                | SceneError::Stage(_)
                | SceneError::Animation(_)
                | SceneError::Persist(_)
                | SceneError::Serialize(_)
                | SceneError::Event(_)
                | SceneError::EndScene(_),
            )
            | Self::Text(_)
            | Self::Typesetting(_)
            | Self::TexEngine(_) => ErrorKind::Scene,
            Self::Scene(SceneError::Camera(_) | SceneError::Integration(_))
            | Self::FileSystem(_)
            | Self::Process(_) => ErrorKind::Render,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(f, "configuration failed: {error}"),
            Self::Geometry(error) => write!(f, "geometry failed: {error}"),
            Self::SpaceOps(error) => write!(f, "space operation failed: {error}"),
            Self::Dash(error) => write!(f, "dash construction failed: {error}"),
            Self::Stage(error) => write!(f, "stage operation failed: {error}"),
            Self::Animation(error) => write!(f, "animation failed: {error}"),
            Self::Scene(error) => write!(f, "{error}"),
            Self::Text(error) => write!(f, "text construction failed: {error}"),
            Self::Typesetting(error) => write!(f, "math construction failed: {error}"),
            Self::TexEngine(error) => write!(f, "math engine failed: {error}"),
            Self::FileSystem(error) => write!(f, "{error}"),
            Self::AssetFetch(error) => write!(f, "{error}"),
            Self::FfmpegLocator(error) => write!(f, "{error}"),
            Self::Process(error) => write!(f, "{error}"),
            Self::Topology(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::Config(error) => error,
            Self::Geometry(error) => error,
            Self::SpaceOps(error) => error,
            Self::Dash(error) => error,
            Self::Stage(error) => error,
            Self::Animation(error) => error,
            Self::Scene(error) => error,
            Self::Text(error) => error,
            Self::Typesetting(error) => error,
            Self::TexEngine(error) => error,
            Self::FileSystem(error) => error,
            Self::AssetFetch(error) => error,
            Self::FfmpegLocator(error) => error,
            Self::Process(error) => error,
            Self::Topology(error) => error,
        })
    }
}

macro_rules! error_from {
    ($source:ty, $variant:ident) => {
        impl From<$source> for Error {
            fn from(error: $source) -> Self {
                Self::$variant(error)
            }
        }
    };
}

error_from!(ConfigError, Config);
error_from!(GeomError, Geometry);
error_from!(SpaceOpsError, SpaceOps);
error_from!(DashError, Dash);
error_from!(StageError, Stage);
error_from!(AnimError, Animation);
error_from!(TextMobjectError, Text);
error_from!(TexMobjectError, Typesetting);
error_from!(TexError, TexEngine);
error_from!(FsError, FileSystem);
error_from!(FetchError, AssetFetch);
error_from!(FfmpegLocatorError, FfmpegLocator);
error_from!(ProcessError, Process);
error_from!(TopologyError, Topology);

impl From<AnimateError> for Error {
    fn from(error: AnimateError) -> Self {
        Self::Animation(error.into())
    }
}

impl From<SceneError> for Error {
    fn from(error: SceneError) -> Self {
        match error {
            SceneError::Stage(error) => Self::Stage(error),
            SceneError::Animation(error) => Self::Animation(error),
            error => Self::Scene(error),
        }
    }
}

/// Scoped construction and playback context for an ordinary Rust scene.
///
/// `Stage` roots newly added mobjects in the real [`Scene`], prepares fluent
/// `.animate` recordings through Choreo, and forwards captures to the host's
/// [`SceneSink`]. It dereferences to Marionette's arena for positional and
/// style operations; the raw arena type remains available as
/// [`crate::mobject::Stage`].
pub struct Stage<'a> {
    scene: &'a mut Scene,
    sink: &'a mut dyn SceneSink,
}

impl Stage<'_> {
    /// Add a detached mobject to the arena and root it in the scene.
    pub fn add(&mut self, mobject: impl Into<Mobject>) -> Result<Mob> {
        self.scene.add_mobject(mobject).map_err(Error::from)
    }

    /// Prepare and play one animation-like value or a simultaneous pair.
    pub fn play(&mut self, animations: impl IntoAnimations) -> Result<SegmentReport> {
        let animations = prepare_animations(animations, self.scene.stage_mut())?;
        self.play_prepared(animations)?.ok_or({
            Error::Scene(SceneError::InvalidLifecycle(
                "a typed play input unexpectedly became empty",
            ))
        })
    }

    /// Prepare an animation without playing it, for a simultaneous group.
    pub fn prepare(&mut self, animation: impl IntoAnimation) -> Result<Box<dyn Animation>> {
        prepare_animation(animation, self.scene.stage_mut()).map_err(Error::from)
    }

    /// Play already-prepared animations simultaneously with default overrides.
    pub fn play_prepared(
        &mut self,
        animations: Vec<Box<dyn Animation>>,
    ) -> Result<Option<SegmentReport>> {
        self.play_prepared_with(animations, PlayOverrides::default())
    }

    /// Play already-prepared animations with explicit play-level overrides.
    pub fn play_prepared_with(
        &mut self,
        animations: Vec<Box<dyn Animation>>,
        overrides: PlayOverrides,
    ) -> Result<Option<SegmentReport>> {
        self.scene
            .play(animations, overrides, self.sink)
            .map_err(Error::from)
    }

    /// Wait for an explicit number of seconds.
    pub fn wait(&mut self, duration: f64) -> Result<SegmentReport> {
        self.scene
            .wait(Some(duration), self.sink)
            .map_err(Error::from)
    }

    /// Wait for the configured default duration.
    pub fn wait_default(&mut self) -> Result<SegmentReport> {
        self.scene.wait(None, self.sink).map_err(Error::from)
    }

    /// End construction through Proscenium's normal early-termination path.
    pub fn end<T>(&mut self) -> Result<T> {
        self.scene.end().map_err(Error::from)
    }

    /// The underlying Proscenium scene for advanced lifecycle operations.
    #[must_use]
    pub fn scene(&self) -> &Scene {
        self.scene
    }

    /// Mutable access to the underlying Proscenium scene.
    pub fn scene_mut(&mut self) -> &mut Scene {
        self.scene
    }

    /// The underlying Marionette arena.
    #[must_use]
    pub fn arena(&self) -> &MobjectStage {
        self.scene.stage()
    }

    /// Mutable access to the underlying Marionette arena.
    pub fn arena_mut(&mut self) -> &mut MobjectStage {
        self.scene.stage_mut()
    }
}

impl std::ops::Deref for Stage<'_> {
    type Target = MobjectStage;

    fn deref(&self) -> &Self::Target {
        self.scene.stage()
    }
}

impl std::ops::DerefMut for Stage<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.scene.stage_mut()
    }
}

/// The compact lifecycle expected by ordinary native Rust scenes.
///
/// Advanced programs can implement [`SceneProgram`] directly. This trait is
/// the common one-hook front door and returns [`Error`] so geometry,
/// typesetting, and animation failures retain their original typed source.
pub trait SceneConstruct {
    /// Stable scene name used in diagnostics and output naming.
    fn name(&self) -> &str {
        "Scene"
    }

    /// Construct and play the scene against the real Proscenium runtime.
    fn construct(&mut self, stage: &mut Stage<'_>) -> Result<()>;
}

struct ProgramAdapter<'a, P: ?Sized> {
    program: &'a mut P,
    front_door_error: Option<Error>,
}

impl<P> SceneProgram for ProgramAdapter<'_, P>
where
    P: SceneConstruct + ?Sized,
{
    fn name(&self) -> &str {
        self.program.name()
    }

    fn construct(
        &mut self,
        scene: &mut Scene,
        sink: &mut dyn SceneSink,
    ) -> std::result::Result<(), SceneError> {
        let mut stage = Stage { scene, sink };
        if let Err(error) = self.program.construct(&mut stage) {
            match error {
                Error::Scene(SceneError::EndScene(signal)) => {
                    return Err(SceneError::EndScene(signal));
                }
                error => {
                    let message = error.to_string();
                    self.front_door_error = Some(error);
                    return Err(IntegrationError::new("rust-api", message).into());
                }
            }
        }
        Ok(())
    }
}

/// A successfully completed native scene.
pub struct CompletedScene {
    scene: Scene,
    report: SceneRunReport,
}

impl CompletedScene {
    /// Runtime report from the real Proscenium lifecycle.
    #[must_use]
    pub const fn report(&self) -> &SceneRunReport {
        &self.report
    }

    /// Final scene state for inspection, persistence, or another host action.
    #[must_use]
    pub const fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Consume the result and take ownership of the final scene state.
    #[must_use]
    pub fn into_scene(self) -> Scene {
        self.scene
    }
}

/// Run a native scene through the real Proscenium lifecycle.
///
/// The returned value retains the final [`Scene`] rather than discarding the
/// arena after execution. Errors from [`SceneConstruct`] are returned in
/// their original variant; lifecycle failures become [`Error::Scene`].
pub fn run_scene<P>(
    program: &mut P,
    config: RuntimeConfig,
    seed: u64,
    sink: &mut dyn SceneSink,
) -> Result<CompletedScene>
where
    P: SceneConstruct + ?Sized,
{
    let mut scene = Scene::new(config, seed)?;
    let mut adapter = ProgramAdapter {
        program,
        front_door_error: None,
    };
    let run = scene.run(&mut adapter, sink);
    if let Some(error) = adapter.front_door_error {
        return Err(error);
    }
    let report = run?;
    Ok(CompletedScene { scene, report })
}
