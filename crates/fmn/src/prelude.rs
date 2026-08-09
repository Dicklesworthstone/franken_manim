//! Curated imports for writing native Rust scenes.
//!
//! This is intentionally smaller than every public symbol in the subsystem
//! crates. Less-common surfaces remain discoverable under `fmn::animation`,
//! `fmn::library`, `fmn::scene`, and the other named modules.

pub use crate::{CompletedScene, Error, ErrorKind, SceneConstruct, Stage, run_scene};
pub use fmn_anim::{
    AnimConfig, AnimError, Animation, AnimationGroup, FramePacket, IntoAnimation, IntoAnimations,
    MoveAlongPath, RateFunc, Rotating, ShowPartial, Succession, Transform, fade_in, fade_out,
    prepare_animation, prepare_animations, rotate, show_creation, write,
};
pub use fmn_config::Config;
pub use fmn_core::color::Srgb;
pub use fmn_core::constants::*;
pub use fmn_core::rng::{Pcg64Dxsm, RngRoot};
pub use fmn_core::types::{Record, Semantic, Vec3};
pub use fmn_library::{
    Annulus, Arc, ArcBetweenPoints, Arrow, Circle, DashedLine, Dot, Ellipse, Line, MarkupText,
    Polygon, Rectangle, RegularPolygon, Square, Style, Tex, TexMobject, TexText, Text, TextMobject,
    VMobject, VStyle,
};
pub use fmn_mobject::{
    AnimBuilder, AnimateArgs, AnimateError, Mob, Mobject, PosTarget, StageError,
};
pub use fmn_scene::{
    CaptureReason, IntegrationError, NullSceneSink, PlayOverrides, RuntimeConfig, Scene,
    SceneError, SceneProgram, SceneRunReport, SceneSink,
};
pub use fmn_tex::TexEngine;
pub use fmn_text::FontBook;
