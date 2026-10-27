//! Curated imports for writing native Rust scenes.
//!
//! Scene-facing animations and mobject constructors are available here;
//! subsystem internals remain under `fmn::animation`, `fmn::library`,
//! `fmn::scene`, and the other named modules.

pub use crate::controls::{
    ButtonBinding, ScalarControlBinding, add_button, add_checkbox, add_enable_disable_button,
    add_slider, add_try_button,
};
pub use crate::exporting::{
    export_camera_bundle, export_camera_bundle_bytes, export_camera_bundle_with_fs,
};
pub use crate::{
    BundleExportError, BundleExportOptions, BundleExportReport, CompletedScene, Error, ErrorKind,
    FfmpegCapability, LibraryError, RenderError, RenderFormat, RenderOptions, RenderReport,
    SceneBundleExport, SceneConstruct, SoundtrackRenderOptions, SoundtrackRenderReport, Stage,
    SvgRenderError, SvgRenderOptions, SvgRenderReport, TexPreflightError, export_bundle,
    export_bundle_bytes, export_bundle_with_fs, render, render_soundtrack,
    render_soundtrack_with_fs, render_svg, render_svg_camera, render_svg_camera_with_fs,
    render_svg_with_fs, render_with_fs, run_scene, run_scene_with_typesetting,
};
pub use fmn_anim::{
    AnimConfig, AnimError, Animation, AnimationGroup, ApplyMethod, DrawBorderThenFill,
    FadeTransform, FramePacket, Homotopy, IntoAnimation, IntoAnimations,
    MaintainPositionRelativeTo, MatchingPartsConfig, MoveAlongPath, PathFunc, PhaseFlow, RateFunc,
    RationalFrameClock, RationalTime, Rotating, ShowIncreasingSubsets, ShowPartial, StartPrep,
    Succession, Timeline, Transform, TransformMatchingParts, TransformMatchingShapes,
    UpdateFromFunc, VFade, VShowPassingFlash, WiggleOutThenIn, add_text_letter_by_letter,
    add_text_word_by_word, animation_on_surrounding_rectangle, apply_complex_function,
    apply_function, apply_matrix, apply_matrix_2d, apply_pointwise_function,
    apply_pointwise_function_to_center, apply_wave, broadcast, circle_indicate, clock_passes_time,
    complex_homotopy, cyclic_replace, fade_in, fade_in_from_point, fade_out, fade_out_to_point,
    fade_to_color, fade_transform, fade_transform_pieces, flash, flash_around, flashy_fade_in,
    focus_on, grow_arrow, grow_from_center, grow_from_edge, grow_from_point, indicate,
    lagged_start, lagged_start_map, move_to_target, prepare_animation, prepare_animations,
    replacement_transform, restore, rotate, rotate_default, scale_in_place, show_creation,
    show_creation_then_destruction, show_creation_then_destruction_around,
    show_creation_then_fade_around, show_creation_then_fade_out, show_increasing_subsets,
    show_passing_flash, show_passing_flash_around, show_submobjects_one_by_one,
    show_surface_creation, shrink_to_center, smoothed_homotopy, swap, transform_from_copy,
    transform_matching_keys, transform_matching_parts, turn_inside_out, uncreate, uncreate_surface,
    v_fade_in, v_fade_in_then_out, v_fade_out, write,
};
pub use fmn_config::Config;
pub use fmn_config::config::{DeterminismMode, Engine, ThreadPolicy};
pub use fmn_core::color::Srgb;
pub use fmn_core::constants::*;
pub use fmn_core::rng::{Pcg64Dxsm, RngRoot};
pub use fmn_core::types::{Record, Semantic, Vec3};
pub use fmn_library::{
    AnimatedBoundary, AnimatedStreamLines, AnnularSector, Annulus, Arc, ArcBetweenPoints, Arrow,
    ArrowTip, Axes, AxisConfig, BarChart, Brace, BraceLabel, BulletedList, BulletedListMobject,
    Button, Checkbox, Circle, Code, CodeTheme, ColorSliders, ComplexPlane, Cone, ControlMob,
    ControlMobject, ControlPanel, ControlPanelMobject, CoordinateFrame, CoordinateFrameError,
    CoordinateSystem, Cube, CubicBezier, Cylinder, DashedLine, DecimalMatrix, DecimalNumber,
    Disk3D, Dodecahedron, Dot, DotCloud, Elbow, Ellipse, EnableDisableButton, FlowAlongEdges,
    FunctionGraph, GraphAnimationError, GraphFlowConfig, GraphLayout, GraphTraversal,
    GraphTraversalConfig, ImageMobject, ImplicitFunction, Integer, IntegerMatrix, LayerKind,
    LayerSpec, LayoutTransition, Line, Line3D, LineAlign, LinearNumberSlider, LiveCoordinateSystem,
    Markdown, MarkdownMobject, MarkupText, Matrix, MatrixMobject, MobjectMatrix, MotionMobject,
    NetworkGraph, NetworkGraphMobject, NeuralNetworkConfig, NeuralNetworkMobject, NumberLine,
    NumberPlane, OldTex, OldTexMobject, PMobject, ParametricCurve, ParametricSurface, Polygon,
    Prism, Prismify, Rectangle, RegularPolygon, RiemannConfig, SGroup, SampleSpace, SamplingBudget,
    ScalarControl, Slider, Sphere, Square, Square3D, StreamLines, StreamLinesMobject, StrokeArrow,
    StrokeProfile, Style, Surface, SurfaceMesh, SurfaceSpec, SurroundingRectangle, TableMobject,
    Tex, TexBindingError, TexMatrix, TexMobject, TexNumberBinding, TexSpanBinding, TexText, Text,
    TextMatchingConfig, TextMobject, Textbox, TexturedSurface, ThreeDAxes, ThreeDModel,
    TimeVaryingVectorField, Title, TitleMobject, Torus, TracedPath, TracingTail,
    TransformMatchingStrings, TransformMatchingTex, TraverseGraph, UnitInterval, VCube, VGroup3D,
    VMobject, VPrism, VStyle, VectorField, VectorFieldMobject, VectorFieldStyle,
    background_rectangle, checkmark, cross, curved_arrow, curved_double_arrow,
    curves_as_submobjects, dashed_vmobject, exmark, glow_dot, glow_dots, group, line_brace,
    p_group, polyline, rounded_rectangle, sector, small_dot, svg_document_mobject, svg_mobject,
    tangent_line, triangle, true_dot, underline, v_group, v_highlight, vector, vectorized_point,
    vmobject_from_svg_path,
};
pub use fmn_mobject::{
    AnimBuilder, AnimateArgs, AnimateError, Mob, Mobject, PosTarget, StageError, UpdaterError,
    UpdaterFailure, UpdaterId,
};
pub use fmn_scene::{
    CaptureReason, IntegrationError, NullSceneSink, PlayOverrides, RuntimeConfig, Scene,
    SceneError, SceneProgram, SceneRunReport, SceneSink,
};
pub use fmn_tex::{TexEngine, TexSession, TypesetRequest, TypesetSessionReport};
pub use fmn_text::FontBook;
