//! The deterministic 3D surface, depth, lighting, texture, and true-dot plane
//! (§10.4–§10.6, fm-0gy).
//!
//! This is part of Lumen's one semantic renderer: commands stay in painter
//! order, while `depth_test` is an opt-in per-fragment operation inside that
//! sequence. Each fine tile owns its own `f32` depth samples, so workers are
//! write-disjoint and thread assignment cannot alter bytes.
//!
//! Surface meshes keep the Reference's fixed UV-grid triangles. Homogeneous
//! clipping precedes perspective division, attributes interpolate
//! perspective-correctly, and untextured Surface lighting is evaluated at
//! vertices then Gouraud-interpolated like the pinned Reference. Textured
//! surfaces retain its fragment-lighting and two-texture `dark_shift`
//! crossfade. `dark_shift` is deliberately absent from [`finalize_color`].

use std::sync::{Mutex, PoisonError};

use fmn_core::color::{LinearRgba, PremulRgba};
use fmn_core::types::Vec3;
use fmn_frame::{FrameBuffer, FrameError, FrameLayout, PixelFormat};

use crate::bin::{ScreenMap, Tiling};
use crate::camera::{Camera, EdgeSampleLimit};
use crate::fill::{self, GradientField, MonoPiece, RationalPiece};
use crate::plan::RenderPlan;
use crate::stroke::{aa_coverage, stroke_rgba_at};
use crate::table::{Segment, Style, reparameterize_arc_length};
use crate::texture::{SamplerPolicy, Texture};

/// Surface's kept `(reflectiveness, gloss, shadow)` defaults.
pub const SURFACE_SHADING: Vec3 = [0.3, 0.2, 0.4];
/// TexturedSurface light/dark crossfade half-width. Not a lighting term.
pub const DARK_SHIFT: f64 = 0.2;
/// GlowDot's kept radial exponent.
pub const GLOW_DOT_FACTOR: f64 = 2.0;
/// True-dot silhouette AA width in output pixels.
pub const TRUE_DOT_AA_WIDTH: f64 = 2.0;

fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn mul(value: Vec3, scalar: f64) -> Vec3 {
    [value[0] * scalar, value[1] * scalar, value[2] * scalar]
}

fn dot(a: Vec3, b: Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length(value: Vec3) -> f64 {
    dot(value, value).sqrt()
}

fn normalize(value: Vec3) -> Option<Vec3> {
    let length = length(value);
    if length == 0.0 || !length.is_finite() {
        None
    } else {
        Some(mul(value, 1.0 / length))
    }
}

fn finite3(value: Vec3) -> bool {
    value.iter().all(|component| component.is_finite())
}

fn rgba_array(color: LinearRgba) -> [f64; 4] {
    [color.r, color.g, color.b, color.a]
}

fn array_rgba(color: [f64; 4]) -> LinearRgba {
    LinearRgba {
        r: color[0],
        g: color[1],
        b: color[2],
        a: color[3],
    }
}

/// The Reference's resilient triangle normal.
///
/// Collinear points first choose the normal in the plane shared with z; a
/// z-axis line falls back to `(0, -1, 0)`.
#[must_use]
pub fn unit_normal(p0: Vec3, p1: Vec3, p2: Vec3) -> Vec3 {
    let v1 = normalize(sub(p1, p0)).unwrap_or([0.0; 3]);
    let v2 = normalize(sub(p2, p0)).unwrap_or([0.0; 3]);
    let first = cross(v1, v2);
    if length(first) > 1e-6 {
        return normalize(first).unwrap_or([0.0, -1.0, 0.0]);
    }
    let combined = add(v1, v2);
    let second = cross(cross(combined, [0.0, 0.0, 1.0]), combined);
    if length(second) > 1e-6 {
        return normalize(second).unwrap_or([0.0, -1.0, 0.0]);
    }
    [0.0, -1.0, 0.0]
}

/// The kept `finalize_color` / `add_light` formula.
///
/// `shading = (reflectiveness, gloss, shadow)`. The Reference evaluates its
/// artistic white/black mixes on sRGB-coded shader inputs. FrankenManim
/// therefore encodes this linear-light input, performs that exact unclamped
/// mix, then decodes the result for its linear compositor. Alpha is unchanged.
/// `dark_shift` is not accepted here because G0-2 proved it belongs only to
/// the two-texture crossfade.
#[must_use]
pub fn finalize_color(
    color: LinearRgba,
    point: Vec3,
    unit_normal: Vec3,
    shading: Vec3,
    light_position: Vec3,
    camera_position: Vec3,
) -> LinearRgba {
    if shading == [0.0; 3] {
        return color;
    }
    let Some(to_camera) = normalize(sub(camera_position, point)) else {
        return color;
    };
    let Some(to_light) = normalize(sub(light_position, point)) else {
        return color;
    };
    let light_to_normal = dot(to_light, unit_normal);
    let mut bright_factor = light_to_normal.max(0.0) * shading[0];
    let incoming = mul(to_light, -1.0);
    let reflection = sub(incoming, mul(unit_normal, 2.0 * dot(incoming, unit_normal)));
    let light_to_camera = dot(reflection, to_camera);
    bright_factor +=
        shading[1] * fmn_dmath::exp(-3.0 * (1.0 - light_to_camera) * (1.0 - light_to_camera));

    let mut encoded = [
        fmn_frame::transfer::srgb_encode(color.r),
        fmn_frame::transfer::srgb_encode(color.g),
        fmn_frame::transfer::srgb_encode(color.b),
    ];
    for component in &mut encoded {
        *component += (1.0 - *component) * bright_factor;
    }
    if light_to_normal < 0.0 {
        let shadow = (-light_to_normal).max(0.0) * shading[2];
        for component in &mut encoded {
            *component += (0.0 - *component) * shadow;
        }
    }
    LinearRgba {
        r: fmn_frame::transfer::srgb_decode(encoded[0]),
        g: fmn_frame::transfer::srgb_decode(encoded[1]),
        b: fmn_frame::transfer::srgb_decode(encoded[2]),
        a: color.a,
    }
}

/// `smoothstep(edge0, edge1, value)`.
#[must_use]
pub fn smoothstep(edge0: f64, edge1: f64, value: f64) -> f64 {
    if edge0 == edge1 {
        return if value >= edge1 { 1.0 } else { 0.0 };
    }
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The true-dot fragment alpha multiplier at normalized radius `r`.
#[must_use]
pub fn true_dot_alpha(r: f64, scaled_aa_width: f64, glow_factor: f64) -> f64 {
    if !r.is_finite() || r > 1.0 || r < 0.0 {
        return 0.0;
    }
    let mut alpha = 1.0;
    if glow_factor > 0.0 {
        alpha *= fmn_dmath::pow(1.0 - r, glow_factor);
    }
    let width = scaled_aa_width.max(f64::EPSILON);
    let edge = ((1.0 - r) / width).clamp(0.0, 1.0);
    alpha * edge * edge * (3.0 - 2.0 * edge)
}

/// One fixed-grid surface vertex.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceVertex {
    /// World-space position.
    pub point: Vec3,
    /// World-space unit-normal direction.
    pub normal: Vec3,
    /// Linear-light straight vertex color.
    pub color: LinearRgba,
    /// Top-left-origin normalized image coordinates.
    pub uv: [f64; 2],
    /// Additional per-vertex opacity.
    pub opacity: f64,
}

impl SurfaceVertex {
    /// An untextured vertex.
    #[must_use]
    pub const fn colored(point: Vec3, normal: Vec3, color: LinearRgba) -> Self {
        Self {
            point,
            normal,
            color,
            uv: [0.0; 2],
            opacity: 1.0,
        }
    }

    /// A textured vertex. Color remains available for future tinting but
    /// texture materials consume only its alpha through `opacity`.
    #[must_use]
    pub const fn textured(point: Vec3, normal: Vec3, uv: [f64; 2], opacity: f64) -> Self {
        Self {
            point,
            normal,
            color: LinearRgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            uv,
            opacity,
        }
    }
}

/// Validated triangle mesh.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceMesh {
    vertices: Vec<SurfaceVertex>,
    indices: Vec<u32>,
    resolution: Option<(u32, u32)>,
}

/// Mesh/job construction refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreeDError {
    /// Triangle index list was not a multiple of three.
    IncompleteTriangle,
    /// An index named no vertex.
    IndexOutOfBounds,
    /// UV resolution did not match the vertex count.
    ResolutionMismatch,
    /// A vertex or draw parameter contained NaN or infinity.
    NonFinite,
    /// Dot radius was not positive.
    InvalidRadius,
    /// Dot anti-alias width was negative.
    InvalidAntiAliasWidth,
    /// A retained-vector command named no instance in its plan.
    InvalidVectorInstance,
    /// Camera clipping left a horizon crossing for the rational fill.
    UnclippedHorizon,
    /// Perspective subdivision hit its declared depth cap.
    PerspectiveToleranceExceeded,
    /// A non-planar fill opted into depth testing, for which no single
    /// fragment-depth surface exists.
    NonPlanarVectorDepth,
    /// A non-planar fill requested an interior color field, whose geometry
    /// cannot be represented in one object-space plane.
    NonPlanarVectorGradient,
    /// A non-planar fill requested point/normal lighting, whose interior has
    /// no single object-space surface.
    NonPlanarVectorShading,
}

impl std::fmt::Display for ThreeDError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IncompleteTriangle => {
                f.write_str("surface index list must contain complete triangles")
            }
            Self::IndexOutOfBounds => f.write_str("surface triangle index exceeds vertex count"),
            Self::ResolutionMismatch => {
                f.write_str("surface UV resolution does not match its vertex count")
            }
            Self::NonFinite => f.write_str("3D draw data must be finite"),
            Self::InvalidRadius => f.write_str("true-dot radius must be positive"),
            Self::InvalidAntiAliasWidth => {
                f.write_str("true-dot anti-alias width must be non-negative")
            }
            Self::InvalidVectorInstance => {
                f.write_str("retained vector draw names no render-plan instance")
            }
            Self::UnclippedHorizon => {
                f.write_str("camera clipping left a vector curve crossing the horizon")
            }
            Self::PerspectiveToleranceExceeded => {
                f.write_str("perspective vector subdivision exceeded its declared depth cap")
            }
            Self::NonPlanarVectorDepth => {
                f.write_str("depth-tested vector fills must lie in one world-space plane")
            }
            Self::NonPlanarVectorGradient => {
                f.write_str("gradient vector fills must lie in one world-space plane")
            }
            Self::NonPlanarVectorShading => {
                f.write_str("shaded vector fills must lie in one world-space plane")
            }
        }
    }
}

impl std::error::Error for ThreeDError {}

impl SurfaceMesh {
    /// Validate an explicitly indexed mesh.
    pub fn new(vertices: Vec<SurfaceVertex>, indices: Vec<u32>) -> Result<Self, ThreeDError> {
        validate_vertices(&vertices)?;
        if !indices.len().is_multiple_of(3) {
            return Err(ThreeDError::IncompleteTriangle);
        }
        if indices
            .iter()
            .any(|&index| index as usize >= vertices.len())
        {
            return Err(ThreeDError::IndexOutOfBounds);
        }
        Ok(Self {
            vertices,
            indices,
            resolution: None,
        })
    }

    /// Apply the Reference's six-index pattern to an `(nu, nv)` UV grid.
    pub fn from_uv_grid(
        vertices: Vec<SurfaceVertex>,
        resolution: (u32, u32),
    ) -> Result<Self, ThreeDError> {
        validate_vertices(&vertices)?;
        let (nu, nv) = resolution;
        let expected = u64::from(nu) * u64::from(nv);
        if usize::try_from(expected).ok() != Some(vertices.len()) {
            return Err(ThreeDError::ResolutionMismatch);
        }
        let mut indices = Vec::new();
        if nu > 0 && nv > 0 {
            let count = u64::from(nu - 1)
                .saturating_mul(u64::from(nv - 1))
                .saturating_mul(6);
            indices.reserve(usize::try_from(count).unwrap_or(0));
            for u in 0..nu - 1 {
                for v in 0..nv - 1 {
                    let top_left = u * nv + v;
                    let bottom_left = (u + 1) * nv + v;
                    let top_right = top_left + 1;
                    let bottom_right = bottom_left + 1;
                    indices.extend_from_slice(&[
                        top_left,
                        bottom_left,
                        top_right,
                        top_right,
                        bottom_left,
                        bottom_right,
                    ]);
                }
            }
        }
        Ok(Self {
            vertices,
            indices,
            resolution: Some(resolution),
        })
    }

    /// Vertices in UV-grid order.
    #[must_use]
    pub fn vertices(&self) -> &[SurfaceVertex] {
        &self.vertices
    }

    /// Triangle indices, three per triangle.
    #[must_use]
    pub fn indices(&self) -> &[u32] {
        &self.indices
    }

    /// Fixed UV resolution, when this mesh came from a grid.
    #[must_use]
    pub const fn resolution(&self) -> Option<(u32, u32)> {
        self.resolution
    }
}

fn validate_vertices(vertices: &[SurfaceVertex]) -> Result<(), ThreeDError> {
    let valid = vertices.iter().all(|vertex| {
        finite3(vertex.point)
            && finite3(vertex.normal)
            && vertex.uv.iter().all(|component| component.is_finite())
            && [
                vertex.color.r,
                vertex.color.g,
                vertex.color.b,
                vertex.color.a,
                vertex.opacity,
            ]
            .iter()
            .all(|component| component.is_finite())
    });
    if valid {
        Ok(())
    } else {
        Err(ThreeDError::NonFinite)
    }
}

/// Surface/image texture material.
#[derive(Debug, Clone, Copy)]
pub struct TextureMaterial<'a> {
    /// Primary texture.
    pub light: &'a Texture,
    /// Optional light-facing alternative.
    pub dark: Option<&'a Texture>,
    /// Per-axis sampler behavior.
    pub sampler: SamplerPolicy,
    /// TexturedSurface discards zero-alpha samples; ImageMobject does not.
    pub discard_transparent: bool,
}

impl<'a> TextureMaterial<'a> {
    /// Image material: one texture, no zero-alpha discard.
    #[must_use]
    pub fn image(texture: &'a Texture) -> Self {
        Self {
            light: texture,
            dark: None,
            sampler: SamplerPolicy::default(),
            discard_transparent: false,
        }
    }

    /// Textured surface with optional dark-side texture.
    #[must_use]
    pub fn surface(light: &'a Texture, dark: Option<&'a Texture>) -> Self {
        Self {
            light,
            dark,
            sampler: SamplerPolicy::default(),
            discard_transparent: true,
        }
    }
}

/// What supplies a surface fragment's base color.
#[derive(Debug, Clone, Copy)]
pub enum SurfaceMaterial<'a> {
    /// Vertex RGBA, lit per vertex and Gouraud-interpolated.
    VertexColor,
    /// Perspective-correct texture sample, lit per fragment.
    Texture(TextureMaterial<'a>),
}

/// One surface draw in painter order.
#[derive(Debug, Clone, Copy)]
pub struct SurfaceDraw<'a> {
    /// Fixed mesh.
    pub mesh: &'a SurfaceMesh,
    /// Vertex or texture material.
    pub material: SurfaceMaterial<'a>,
    /// `(reflectiveness, gloss, shadow)`.
    pub shading: Vec3,
    /// Float camera/fixed-frame mix.
    pub is_fixed_in_frame: f64,
    /// Four world-space clip planes.
    pub clip_planes: [[f64; 4]; 4],
    /// Opt-in depth test/write.
    pub depth_test: bool,
}

impl<'a> SurfaceDraw<'a> {
    /// Reference Surface defaults.
    #[must_use]
    pub const fn new(mesh: &'a SurfaceMesh) -> Self {
        Self {
            mesh,
            material: SurfaceMaterial::VertexColor,
            shading: SURFACE_SHADING,
            is_fixed_in_frame: 0.0,
            clip_planes: [[0.0; 4]; 4],
            depth_test: true,
        }
    }

    /// Image defaults: unlit texture in painter order.
    #[must_use]
    pub const fn image(mesh: &'a SurfaceMesh, texture: &'a Texture) -> Self {
        Self {
            mesh,
            material: SurfaceMaterial::Texture(TextureMaterial {
                light: texture,
                dark: None,
                sampler: SamplerPolicy {
                    wrap_u: crate::texture::TextureWrap::Repeat,
                    wrap_v: crate::texture::TextureWrap::Repeat,
                },
                discard_transparent: false,
            }),
            shading: [0.0; 3],
            is_fixed_in_frame: 0.0,
            clip_planes: [[0.0; 4]; 4],
            depth_test: false,
        }
    }
}

/// A camera-facing true dot / glow dot.
#[derive(Debug, Clone, Copy)]
pub struct TrueDotDraw {
    /// World-space center.
    pub center: Vec3,
    /// World-space radius.
    pub radius: f64,
    /// Linear-light straight color.
    pub color: LinearRgba,
    /// Zero for TrueDot/DotCloud; two for GlowDot.
    pub glow_factor: f64,
    /// Silhouette anti-alias width in output pixels.
    pub anti_alias_width: f64,
    /// `(reflectiveness, gloss, shadow)`.
    pub shading: Vec3,
    /// Float camera/fixed-frame mix.
    pub is_fixed_in_frame: f64,
    /// Four world-space clip planes.
    pub clip_planes: [[f64; 4]; 4],
    /// Opt-in depth test/write.
    pub depth_test: bool,
}

/// One retained vector instance inserted into the shared 3D painter sequence.
///
/// Geometry and style remain owned by [`RenderPlan`]. Construction carries only
/// the painter-sequence instance index; [`ThreeDJob`] derives camera-clipped
/// rational fill and true-distance stroke data for the captured camera.
#[derive(Debug, Clone, Copy)]
pub struct VectorDraw<'a> {
    plan: &'a RenderPlan,
    instance: u32,
}

impl<'a> VectorDraw<'a> {
    /// Refer to one painter-ordered retained instance.
    #[must_use]
    pub const fn new(plan: &'a RenderPlan, instance: u32) -> Self {
        Self { plan, instance }
    }

    /// Retained instance index.
    #[must_use]
    pub const fn instance(self) -> u32 {
        self.instance
    }
}

impl TrueDotDraw {
    /// An unlit, hard-interior true dot.
    #[must_use]
    pub const fn new(center: Vec3, radius: f64, color: LinearRgba) -> Self {
        Self {
            center,
            radius,
            color,
            glow_factor: 0.0,
            anti_alias_width: TRUE_DOT_AA_WIDTH,
            shading: [0.0; 3],
            is_fixed_in_frame: 0.0,
            clip_planes: [[0.0; 4]; 4],
            depth_test: false,
        }
    }

    /// GlowDot's kept factor.
    #[must_use]
    pub const fn glow(center: Vec3, radius: f64, color: LinearRgba) -> Self {
        Self {
            glow_factor: GLOW_DOT_FACTOR,
            ..Self::new(center, radius, color)
        }
    }
}

/// One command in the 3D painter sequence.
#[derive(Debug, Clone, Copy)]
pub enum ThreeDDraw<'a> {
    /// Analytic fill/stroke from the retained vector IR.
    Vector(VectorDraw<'a>),
    /// Indexed surface/image triangles.
    Surface(SurfaceDraw<'a>),
    /// Camera-facing radial dot.
    TrueDot(TrueDotDraw),
}

#[derive(Debug, Clone, Copy)]
struct Attributes {
    world: Vec3,
    normal: Vec3,
    color: [f64; 4],
    uv: [f64; 2],
    opacity: f64,
}

impl Attributes {
    fn lerp(self, other: Self, alpha: f64) -> Self {
        let lerp = |a: f64, b: f64| a + (b - a) * alpha;
        let mut world = [0.0; 3];
        let mut normal = [0.0; 3];
        let mut color = [0.0; 4];
        let mut uv = [0.0; 2];
        for index in 0..3 {
            world[index] = lerp(self.world[index], other.world[index]);
            normal[index] = lerp(self.normal[index], other.normal[index]);
        }
        for (index, component) in color.iter_mut().enumerate() {
            *component = lerp(self.color[index], other.color[index]);
        }
        for (index, component) in uv.iter_mut().enumerate() {
            *component = lerp(self.uv[index], other.uv[index]);
        }
        Self {
            world,
            normal,
            color,
            uv,
            opacity: lerp(self.opacity, other.opacity),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct WorldVertex {
    attributes: Attributes,
}

#[derive(Debug, Clone, Copy)]
struct ProjectedVertex {
    attributes: Attributes,
    clip: [f64; 4],
}

impl ProjectedVertex {
    fn lerp(self, other: Self, alpha: f64) -> Self {
        let mut clip = [0.0; 4];
        for (index, component) in clip.iter_mut().enumerate() {
            *component = self.clip[index] + (other.clip[index] - self.clip[index]) * alpha;
        }
        Self {
            attributes: self.attributes.lerp(other.attributes, alpha),
            clip,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RasterVertex {
    attributes: Attributes,
    screen: [f64; 2],
    inverse_w: f64,
    ndc_z: f64,
}

#[derive(Debug, Clone)]
struct RasterTriangle {
    vertices: [RasterVertex; 3],
    bounds: [f64; 4],
}

const PERSPECTIVE_VECTOR_TOLERANCE_PX: f64 = 1.0 / 256.0;

#[derive(Debug, Clone)]
struct ProjectedCurvePiece {
    screen: Segment,
    world: [Vec3; 3],
}

#[derive(Debug, Clone, Copy)]
struct CurvePosition {
    world: [Vec3; 3],
    t: f64,
    s: f64,
}

#[derive(Debug, Clone)]
struct PlanarGradientField {
    field: GradientField,
    origin: Vec3,
    u: Vec3,
    v: Vec3,
}

#[derive(Debug, Clone)]
struct CompiledVector {
    fill: Vec<MonoPiece>,
    curves: Vec<ProjectedCurvePiece>,
    joins: Vec<crate::stroke::JoinWedge>,
    field: Option<PlanarGradientField>,
    style: Style,
    normal: Vec3,
    fill_plane: Option<[f64; 4]>,
    draws_fill: bool,
    draws_stroke: bool,
}

#[derive(Debug, Clone, Copy)]
enum Shader<'a> {
    Gouraud,
    Texture {
        material: TextureMaterial<'a>,
        shading: Vec3,
        light_position: Vec3,
        camera_position: Vec3,
    },
    Dot {
        center: Vec3,
        radius: f64,
        color: LinearRgba,
        glow_factor: f64,
        shading: Vec3,
        to_camera: Vec3,
        scaled_aa_width: f64,
        light_position: Vec3,
        camera_position: Vec3,
    },
}

#[derive(Debug, Clone)]
struct CompiledDraw<'a> {
    primitive: CompiledPrimitive<'a>,
    depth_test: bool,
    bounds: Option<[f64; 4]>,
}

#[derive(Debug, Clone)]
enum CompiledPrimitive<'a> {
    Triangles {
        triangles: Vec<RasterTriangle>,
        shader: Shader<'a>,
    },
    Vector(Box<CompiledVector>),
}

/// Camera-bound, clipped, tiled 3D frame.
#[derive(Debug)]
pub struct ThreeDJob<'a> {
    camera: &'a Camera,
    draws: Vec<CompiledDraw<'a>>,
    tiling: Tiling,
    sample_grid: u32,
    camera_revision: u64,
}

impl<'a> ThreeDJob<'a> {
    /// Compile painter-ordered primitives against one immutable camera.
    pub fn new(
        camera: &'a Camera,
        draws: &[ThreeDDraw<'a>],
        tiling: Tiling,
    ) -> Result<Self, ThreeDError> {
        let mut compiled = Vec::with_capacity(draws.len());
        for draw in draws {
            compiled.push(match *draw {
                ThreeDDraw::Vector(vector) => compile_vector(camera, vector)?,
                ThreeDDraw::Surface(surface) => compile_surface(camera, surface)?,
                ThreeDDraw::TrueDot(dot) => compile_dot(camera, dot)?,
            });
        }
        let sample_grid = match camera.edge_sample_limit() {
            EdgeSampleLimit::Native => 1,
            EdgeSampleLimit::TwoByTwo => 2,
            EdgeSampleLimit::FourByFour => 4,
        };
        Ok(Self {
            camera,
            draws: compiled,
            tiling,
            sample_grid,
            camera_revision: camera.revision(),
        })
    }

    /// Number of painter-sequence commands, including clipped-empty draws.
    #[must_use]
    pub fn draw_count(&self) -> usize {
        self.draws.len()
    }

    /// Fine-tile schedule.
    #[must_use]
    pub const fn tiling(&self) -> Tiling {
        self.tiling
    }

    /// Camera revision that owns every projected/vector/triangle derivation.
    #[must_use]
    pub const fn camera_revision(&self) -> u64 {
        self.camera_revision
    }

    /// Raw-frame layout.
    pub fn layout(&self) -> Result<FrameLayout, FrameError> {
        FrameLayout::tight(
            PixelFormat::Rgba16F,
            self.camera.pixel_width(),
            self.camera.pixel_height(),
        )
    }

    /// Render into a new frame.
    pub fn render(&self, threads: usize) -> Result<FrameBuffer, FrameError> {
        let mut frame = FrameBuffer::new(self.layout()?);
        self.render_into(threads, &mut frame)?;
        Ok(frame)
    }

    /// Render into a caller-owned Rgba16F frame.
    pub fn render_into(
        &self,
        threads: usize,
        destination: &mut FrameBuffer,
    ) -> Result<(), FrameError> {
        if destination.layout().format() != PixelFormat::Rgba16F {
            return Err(FrameError::FormatMismatch {
                expected: "Rgba16F raw frame",
                got: destination.layout().format(),
            });
        }
        if destination.layout().width() != self.camera.pixel_width()
            || destination.layout().height() != self.camera.pixel_height()
        {
            return Err(FrameError::DimensionMismatch);
        }

        let tile = self.tiling.fine_tile.max(1) as usize;
        let stride = destination.layout().stride(0);
        let band_bytes = stride.checked_mul(tile).ok_or(FrameError::TooLarge)?;
        let plane = destination.plane_mut(0);
        let mut bands: Vec<(usize, &mut [u8])> = plane.chunks_mut(band_bytes).enumerate().collect();

        if threads <= 1 {
            let mut scratch = TileScratch::new(tile, self.sample_grid);
            for (band, bytes) in bands {
                self.render_band(&mut scratch, band, bytes, stride);
            }
            return Ok(());
        }

        bands.reverse();
        let queue = Mutex::new(bands);
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| {
                    let mut scratch = TileScratch::new(tile, self.sample_grid);
                    loop {
                        let next = queue.lock().unwrap_or_else(PoisonError::into_inner).pop();
                        let Some((band, bytes)) = next else { break };
                        self.render_band(&mut scratch, band, bytes, stride);
                    }
                });
            }
        });
        Ok(())
    }

    fn render_band(&self, scratch: &mut TileScratch, band: usize, bytes: &mut [u8], stride: usize) {
        let tile = self.tiling.fine_tile.max(1);
        let width = self.camera.pixel_width();
        let height = self.camera.pixel_height();
        let y0 = band as u32 * tile;
        let y1 = (y0 + tile).min(height);
        let columns = width.div_ceil(tile);
        for column in 0..columns {
            let x0 = column * tile;
            let x1 = (x0 + tile).min(width);
            self.render_tile(scratch, [x0, y0, x1, y1], bytes, stride);
        }
    }

    fn render_tile(
        &self,
        scratch: &mut TileScratch,
        rectangle: [u32; 4],
        bytes: &mut [u8],
        stride: usize,
    ) {
        let [x0, y0, x1, y1] = rectangle;
        let width = (x1 - x0) as usize;
        let height = (y1 - y0) as usize;
        if width == 0 || height == 0 {
            return;
        }
        scratch.clear(width, height, self.camera.background());
        if self.sample_grid > 1 {
            for draw in &self.draws {
                if !bounds_intersect(draw.bounds, rectangle) {
                    continue;
                }
                match &draw.primitive {
                    CompiledPrimitive::Triangles { triangles, .. } => {
                        for triangle in triangles {
                            scratch.mark_triangle_boundary(triangle, rectangle, width, height);
                        }
                    }
                    CompiledPrimitive::Vector(vector) => {
                        scratch.mark_vector_boundaries(
                            self.camera,
                            vector,
                            rectangle,
                            width,
                            height,
                        );
                    }
                }
            }
        }

        for draw in &self.draws {
            if !bounds_intersect(draw.bounds, rectangle) {
                continue;
            }
            match &draw.primitive {
                CompiledPrimitive::Triangles { triangles, shader } => {
                    for triangle in triangles {
                        scratch.raster_triangle(
                            triangle,
                            *shader,
                            draw.depth_test,
                            rectangle,
                            width,
                            height,
                        );
                    }
                }
                CompiledPrimitive::Vector(vector) => {
                    scratch.raster_vector(
                        self.camera,
                        vector,
                        draw.depth_test,
                        rectangle,
                        width,
                        height,
                    );
                }
            }
        }

        for local_y in 0..height {
            let row = &mut bytes[local_y * stride..];
            for local_x in 0..width {
                let pixel = scratch.resolve(local_x, local_y, width);
                let offset = (x0 as usize + local_x) * 8;
                write_pixel(pixel, &mut row[offset..offset + 8]);
            }
        }
    }
}

fn compile_vector<'a>(
    camera: &'a Camera,
    draw: VectorDraw<'a>,
) -> Result<CompiledDraw<'a>, ThreeDError> {
    let instance = draw
        .plan
        .shapes()
        .instances()
        .get(draw.instance as usize)
        .ok_or(ThreeDError::InvalidVectorInstance)?;
    let shape = draw
        .plan
        .shapes()
        .shape(instance.shape)
        .ok_or(ThreeDError::InvalidVectorInstance)?;
    let style = draw
        .plan
        .styles()
        .get(instance.style)
        .copied()
        .ok_or(ThreeDError::InvalidVectorInstance)?;
    if !style.is_fixed_in_frame.is_finite()
        || !finite3(style.shading)
        || style
            .clip_planes
            .iter()
            .flatten()
            .any(|value| !value.is_finite())
    {
        return Err(ThreeDError::NonFinite);
    }

    let all = draw.plan.segments();
    let lo = (shape.first_segment as usize).min(all.len());
    let hi = (lo + shape.segment_count as usize).min(all.len());
    let source = &all[lo..hi];
    // Apply the linear map before deriving the path normal, but keep translation
    // out of that calculation so two translated instances cannot acquire
    // different floating-point normals through cancellation of large
    // coordinates.
    let mut linear_segments: Vec<Segment> = source
        .iter()
        .map(|segment| Segment {
            p0: instance.placement.apply_vector(segment.p0),
            p1: instance.placement.apply_vector(segment.p1),
            p2: instance.placement.apply_vector(segment.p2),
            s0: segment.s0,
            s1: segment.s1,
        })
        .collect();
    reparameterize_arc_length(&mut linear_segments);
    let normal = shape_unit_normal(&linear_segments, &shape.subpath_starts);
    let translation = instance.placement.translation();
    let world_segments: Vec<Segment> = linear_segments
        .iter()
        .map(|segment| Segment {
            p0: add(segment.p0, translation),
            p1: add(segment.p1, translation),
            p2: add(segment.p2, translation),
            s0: segment.s0,
            s1: segment.s1,
        })
        .collect();

    let draws_fill = style.fill_rgba[3] > 0.0 || style.fill_rgba_end[3] > 0.0;
    let draws_stroke = (style.stroke_width > 0.0 || style.stroke_width_end > 0.0)
        && (style.stroke_rgba[3] > 0.0 || style.stroke_rgba_end[3] > 0.0);
    let mut fill_pieces = Vec::new();
    let mut curves = Vec::new();
    let mut curve_starts = Vec::new();
    let starts = &shape.subpath_starts;
    for (subpath, &start) in starts.iter().enumerate() {
        let end = starts
            .get(subpath + 1)
            .copied()
            .unwrap_or(world_segments.len() as u32)
            .min(world_segments.len() as u32);
        let subpath = &world_segments[start as usize..end as usize];
        if draws_fill {
            let controls: Vec<[Vec3; 3]> = subpath
                .iter()
                .map(|segment| [segment.p0, segment.p1, segment.p2])
                .collect();
            for clipped in camera
                .project_fill_contour(&controls, style.is_fixed_in_frame, style.clip_planes)
                .map_err(|_| ThreeDError::NonFinite)?
            {
                let rational = RationalPiece {
                    p: clipped.screen_controls(camera.pixel_shape()),
                };
                let report = fill::append_rational(
                    &rational,
                    PERSPECTIVE_VECTOR_TOLERANCE_PX,
                    &mut fill_pieces,
                )
                .map_err(|_| ThreeDError::UnclippedHorizon)?;
                if report.capped {
                    return Err(ThreeDError::PerspectiveToleranceExceeded);
                }
            }
        }

        let mut run_last_world: Option<Vec3> = None;
        for segment in subpath {
            let projected = camera
                .project_quadratic(
                    [segment.p0, segment.p1, segment.p2],
                    style.is_fixed_in_frame,
                    // User planes clip the stroke *surface* below. Keeping the
                    // full camera-visible centerline here matters at oblique
                    // cuts: distance to a centerline point just outside the
                    // plane can still define a legal fragment just inside it.
                    [[0.0; 4]; 4],
                )
                .map_err(|_| ThreeDError::NonFinite)?;
            for clipped in projected {
                let rational = RationalPiece {
                    p: clipped.screen_controls(camera.pixel_shape()),
                };
                let starts_run =
                    run_last_world.is_none_or(|point| !points_close(point, clipped.world[0]));
                if starts_run {
                    curve_starts.push(curves.len() as u32);
                }
                let s0 = segment_s_at(segment, clipped.source_t[0]);
                let s1 = segment_s_at(segment, clipped.source_t[1]);
                append_projected_curves(rational, clipped.world, s0, s1, 0, &mut curves)?;
                run_last_world = Some(clipped.world[2]);
            }
        }
    }

    let screen_segments: Vec<Segment> = curves.iter().map(|piece| piece.screen).collect();
    let mut joins = crate::stroke::join_wedges(
        &screen_segments,
        &curve_starts,
        &style,
        unit_screen_map(),
        [0.0; 2],
    );
    for join in &mut joins {
        if let Some(curve) = curves.iter().min_by(|a, b| {
            let distance = |piece: &ProjectedCurvePiece| {
                let dx = piece.screen.p2[0] - join.anchor[0];
                let dy = piece.screen.p2[1] - join.anchor[1];
                dx * dx + dy * dy
            };
            distance(a).total_cmp(&distance(b))
        }) {
            join.half_width = projected_width_toward(
                camera,
                &style,
                normal,
                CurvePosition {
                    world: curve.world,
                    t: 1.0,
                    s: curve.screen.s1,
                },
                [
                    join.anchor[0] + join.bisector[0],
                    join.anchor[1] + join.bisector[1],
                ],
                None,
            );
        }
    }
    let planar = vector_is_planar(&world_segments, normal);
    let fill_plane = if planar {
        world_segments
            .first()
            .map(|segment| [normal[0], normal[1], normal[2], -dot(normal, segment.p0)])
    } else {
        None
    };
    if style.depth_test && draws_fill && !planar {
        return Err(ThreeDError::NonPlanarVectorDepth);
    }
    if draws_fill && !fill::fill_is_flat(&style) && !planar {
        return Err(ThreeDError::NonPlanarVectorGradient);
    }
    if draws_fill && style.shading != [0.0; 3] && !planar {
        return Err(ThreeDError::NonPlanarVectorShading);
    }
    let field = if draws_fill && !fill::fill_is_flat(&style) {
        fill_plane.and_then(|plane| PlanarGradientField::build(&world_segments, plane))
    } else {
        None
    };
    let bounds = vector_bounds(camera, &style, normal, &fill_pieces, &curves);
    Ok(CompiledDraw {
        primitive: CompiledPrimitive::Vector(Box::new(CompiledVector {
            fill: fill_pieces,
            curves,
            joins,
            field,
            style,
            normal,
            fill_plane,
            draws_fill,
            draws_stroke,
        })),
        depth_test: style.depth_test,
        bounds,
    })
}

fn unit_screen_map() -> ScreenMap {
    ScreenMap {
        scale: 1.0,
        origin: [0.0; 2],
    }
}

fn points_close(a: Vec3, b: Vec3) -> bool {
    let scale = a
        .iter()
        .chain(&b)
        .fold(1.0f64, |largest, value| largest.max(value.abs()));
    sub(a, b)
        .iter()
        .all(|value| value.abs() <= 128.0 * f64::EPSILON * scale)
}

fn segment_s_at(segment: &Segment, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let total = fmn_geom::arclength::quadratic_arc_length(segment.p0, segment.p1, segment.p2);
    if total <= 0.0 {
        return segment.s0;
    }
    let partial =
        fmn_geom::bezier::partial_quadratic(&[segment.p0, segment.p1, segment.p2], 0.0, t);
    let fraction =
        fmn_geom::arclength::quadratic_arc_length(partial[0], partial[1], partial[2]) / total;
    segment.s0 + (segment.s1 - segment.s0) * fraction.clamp(0.0, 1.0)
}

fn append_projected_curves(
    rational: RationalPiece,
    world: [Vec3; 3],
    s0: f64,
    s1: f64,
    depth: u32,
    output: &mut Vec<ProjectedCurvePiece>,
) -> Result<(), ThreeDError> {
    let error = rational.deviation_px();
    if error <= PERSPECTIVE_VECTOR_TOLERANCE_PX {
        let curve = rational.integral_approximation();
        output.push(ProjectedCurvePiece {
            screen: Segment {
                p0: [curve.p0[0], curve.p0[1], 0.0],
                p1: [curve.p1[0], curve.p1[1], 0.0],
                p2: [curve.p2[0], curve.p2[1], 0.0],
                s0,
                s1,
            },
            world,
        });
        return Ok(());
    }
    if depth >= fill::FLATTEN_MAX_DEPTH {
        return Err(ThreeDError::PerspectiveToleranceExceeded);
    }
    let (rational_left, rational_right) = rational.split(0.5);
    let (world_left, world_right) = split_world_quadratic(world, 0.5);
    let left_length =
        fmn_geom::arclength::quadratic_arc_length(world_left[0], world_left[1], world_left[2]);
    let right_length =
        fmn_geom::arclength::quadratic_arc_length(world_right[0], world_right[1], world_right[2]);
    let fraction = if left_length + right_length > 0.0 {
        left_length / (left_length + right_length)
    } else {
        0.5
    };
    let middle = s0 + (s1 - s0) * fraction;
    append_projected_curves(rational_left, world_left, s0, middle, depth + 1, output)?;
    append_projected_curves(rational_right, world_right, middle, s1, depth + 1, output)
}

fn split_world_quadratic(world: [Vec3; 3], t: f64) -> ([Vec3; 3], [Vec3; 3]) {
    let lerp = |a: Vec3, b: Vec3| add(a, mul(sub(b, a), t));
    let q0 = lerp(world[0], world[1]);
    let q1 = lerp(world[1], world[2]);
    let r = lerp(q0, q1);
    ([world[0], q0, r], [r, q1, world[2]])
}

fn shape_unit_normal(segments: &[Segment], subpath_starts: &[u32]) -> Vec3 {
    let mut area = [0.0; 3];
    for (subpath, &start) in subpath_starts.iter().enumerate() {
        let end = subpath_starts
            .get(subpath + 1)
            .copied()
            .unwrap_or(segments.len() as u32);
        let lo = (start as usize).min(segments.len());
        let hi = (end as usize).min(segments.len());
        if lo >= hi {
            continue;
        }
        let mut anchors: Vec<Vec3> = segments[lo..hi].iter().map(|segment| segment.p0).collect();
        anchors.push(segments[hi - 1].p2);
        for index in 0..anchors.len() {
            let p0 = anchors[index];
            let p1 = anchors[(index + 1) % anchors.len()];
            area[0] += 0.5 * (p0[1] + p1[1]) * (p1[2] - p0[2]);
            area[1] += 0.5 * (p0[2] + p1[2]) * (p1[0] - p0[0]);
            area[2] += 0.5 * (p0[0] + p1[0]) * (p1[1] - p0[1]);
        }
    }
    normalize(area).unwrap_or_else(|| {
        segments.first().map_or([0.0, 0.0, 1.0], |segment| {
            unit_normal(segment.p0, segment.p1, segment.p2)
        })
    })
}

fn vector_is_planar(segments: &[Segment], normal: Vec3) -> bool {
    let Some(origin) = segments.first().map(|segment| segment.p0) else {
        return true;
    };
    let scale = segments
        .iter()
        .flat_map(|segment| [segment.p0, segment.p1, segment.p2])
        .flat_map(|point| sub(point, origin))
        .fold(1.0f64, |largest, value| largest.max(value.abs()));
    let tolerance = 1e-10 * scale;
    segments
        .iter()
        .flat_map(|segment| [segment.p0, segment.p1, segment.p2])
        .all(|point| dot(normal, sub(point, origin)).abs() <= tolerance)
}

impl PlanarGradientField {
    fn build(segments: &[Segment], plane: [f64; 4]) -> Option<Self> {
        let normal = [plane[0], plane[1], plane[2]];
        let absolute = [normal[0].abs(), normal[1].abs(), normal[2].abs()];
        let reference = if absolute[0] <= absolute[1] && absolute[0] <= absolute[2] {
            [1.0, 0.0, 0.0]
        } else if absolute[1] <= absolute[2] {
            [0.0, 1.0, 0.0]
        } else {
            [0.0, 0.0, 1.0]
        };
        let u = normalize(cross(reference, normal))?;
        let v = normalize(cross(normal, u))?;
        let origin = mul(normal, -plane[3]);
        let coordinates = |point: Vec3| {
            let relative = sub(point, origin);
            [dot(relative, u), dot(relative, v), 0.0]
        };
        let planar_segments: Vec<Segment> = segments
            .iter()
            .map(|segment| Segment {
                p0: coordinates(segment.p0),
                p1: coordinates(segment.p1),
                p2: coordinates(segment.p2),
                s0: segment.s0,
                s1: segment.s1,
            })
            .collect();
        Some(Self {
            field: GradientField::build(&planar_segments, unit_screen_map()),
            origin,
            u,
            v,
        })
    }

    fn param_at(&self, world: Vec3) -> f64 {
        let relative = sub(world, self.origin);
        self.field
            .param_at([dot(relative, self.u), dot(relative, self.v)], [0.0; 2])
    }
}

fn vector_bounds(
    camera: &Camera,
    style: &Style,
    normal: Vec3,
    fill: &[MonoPiece],
    curves: &[ProjectedCurvePiece],
) -> Option<[f64; 4]> {
    let mut bounds = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for piece in fill {
        for point in [piece.p0, piece.p1, piece.p2] {
            bounds[0] = bounds[0].min(point[0]);
            bounds[1] = bounds[1].min(point[1]);
            bounds[2] = bounds[2].max(point[0]);
            bounds[3] = bounds[3].max(point[1]);
        }
    }
    let mut stroke_pad = 0.0f64;
    for curve in curves {
        for point in [curve.screen.p0, curve.screen.p1, curve.screen.p2] {
            bounds[0] = bounds[0].min(point[0]);
            bounds[1] = bounds[1].min(point[1]);
            bounds[2] = bounds[2].max(point[0]);
            bounds[3] = bounds[3].max(point[1]);
        }
        for t in [0.0, 0.5, 1.0] {
            let s = curve.screen.s0 + (curve.screen.s1 - curve.screen.s0) * t;
            let widths = projected_half_widths(camera, style, normal, curve.world, t, s);
            stroke_pad = stroke_pad.max(widths[0]).max(widths[1]);
        }
    }
    let visible_stroke = (style.stroke_width > 0.0 || style.stroke_width_end > 0.0)
        && (style.stroke_rgba[3] > 0.0 || style.stroke_rgba_end[3] > 0.0);
    if visible_stroke {
        // Perspective width is directional and may peak between the retained
        // curve samples. Until the camera table carries an analytic slab for
        // that rational offset field, the viewport is the only fail-closed
        // bound: a sampled maximum is an optimization that can clip a legal
        // stroke. Tile-local distance tests still reject untouched pixels.
        bounds[0] = bounds[0].min(0.0);
        bounds[1] = bounds[1].min(0.0);
        bounds[2] = bounds[2].max(f64::from(camera.pixel_width()));
        bounds[3] = bounds[3].max(f64::from(camera.pixel_height()));
    }
    if !bounds[0].is_finite() {
        return None;
    }
    let pad = stroke_pad + f64::from(style.anti_alias_width);
    Some([
        bounds[0] - pad,
        bounds[1] - pad,
        bounds[2] + pad,
        bounds[3] + pad,
    ])
}

fn compile_surface<'a>(
    camera: &'a Camera,
    draw: SurfaceDraw<'a>,
) -> Result<CompiledDraw<'a>, ThreeDError> {
    if !draw.is_fixed_in_frame.is_finite()
        || !finite3(draw.shading)
        || draw
            .clip_planes
            .iter()
            .flatten()
            .any(|value| !value.is_finite())
    {
        return Err(ThreeDError::NonFinite);
    }
    let light = camera.light_source_position();
    let camera_position = camera.location();
    let mut triangles = Vec::new();
    for triangle in draw.mesh.indices.as_chunks::<3>().0 {
        let vertices = [
            draw.mesh.vertices[triangle[0] as usize],
            draw.mesh.vertices[triangle[1] as usize],
            draw.mesh.vertices[triangle[2] as usize],
        ];
        let world = vertices.map(|vertex| {
            // Both surface vertex shaders normalize `d_normal_point - point`
            // before emitting the varying. TexturedSurface then interpolates
            // that varying without normalizing it again in the fragment
            // shader, which is subtle but visible on coarse curved grids.
            let unit_normal = normalize(vertex.normal).unwrap_or([0.0, -1.0, 0.0]);
            let color = match draw.material {
                SurfaceMaterial::VertexColor => finalize_color(
                    vertex.color,
                    vertex.point,
                    unit_normal,
                    draw.shading,
                    light,
                    camera_position,
                ),
                SurfaceMaterial::Texture(_) => vertex.color,
            };
            WorldVertex {
                attributes: Attributes {
                    world: vertex.point,
                    normal: unit_normal,
                    color: rgba_array(color),
                    uv: vertex.uv,
                    opacity: vertex.opacity,
                },
            }
        });
        append_clipped_triangle(
            camera,
            &world,
            draw.is_fixed_in_frame,
            draw.clip_planes,
            &mut triangles,
        );
    }
    let shader = match draw.material {
        SurfaceMaterial::VertexColor => Shader::Gouraud,
        SurfaceMaterial::Texture(material) => Shader::Texture {
            material,
            shading: draw.shading,
            light_position: light,
            camera_position,
        },
    };
    Ok(CompiledDraw {
        bounds: union_bounds(&triangles),
        primitive: CompiledPrimitive::Triangles { triangles, shader },
        depth_test: draw.depth_test,
    })
}

fn compile_dot<'a>(camera: &'a Camera, draw: TrueDotDraw) -> Result<CompiledDraw<'a>, ThreeDError> {
    if !finite3(draw.center)
        || !finite3(draw.shading)
        || ![
            draw.radius,
            draw.glow_factor,
            draw.anti_alias_width,
            draw.is_fixed_in_frame,
            draw.color.r,
            draw.color.g,
            draw.color.b,
            draw.color.a,
        ]
        .iter()
        .all(|value| value.is_finite())
        || draw
            .clip_planes
            .iter()
            .flatten()
            .any(|value| !value.is_finite())
    {
        return Err(ThreeDError::NonFinite);
    }
    if draw.radius <= 0.0 {
        return Err(ThreeDError::InvalidRadius);
    }
    if draw.anti_alias_width < 0.0 {
        return Err(ThreeDError::InvalidAntiAliasWidth);
    }
    let to_camera = normalize(sub(camera.location(), draw.center)).unwrap_or([0.0, 0.0, 1.0]);
    let mut right = normalize(cross([0.0, 1.0, 1.0], to_camera));
    if right.is_none() {
        right = normalize(cross([1.0, 0.0, 0.0], to_camera));
    }
    let right = mul(right.unwrap_or([1.0, 0.0, 0.0]), draw.radius);
    let up = mul(
        normalize(cross(to_camera, right)).unwrap_or([0.0, 1.0, 0.0]),
        draw.radius,
    );
    let corner = |i: f64, j: f64| WorldVertex {
        attributes: Attributes {
            world: add(draw.center, add(mul(right, i), mul(up, j))),
            normal: to_camera,
            color: rgba_array(draw.color),
            uv: [i, j],
            opacity: 1.0,
        },
    };
    // Geometry-shader triangle-strip order: (-1,-1),(-1,1),(1,-1),(1,1).
    let corners = [
        corner(-1.0, -1.0),
        corner(-1.0, 1.0),
        corner(1.0, -1.0),
        corner(1.0, 1.0),
    ];
    let mut triangles = Vec::new();
    for indices in [[0, 1, 2], [2, 1, 3]] {
        let triangle = indices.map(|index| corners[index]);
        append_clipped_triangle(
            camera,
            &triangle,
            draw.is_fixed_in_frame,
            draw.clip_planes,
            &mut triangles,
        );
    }
    Ok(CompiledDraw {
        bounds: union_bounds(&triangles),
        primitive: CompiledPrimitive::Triangles {
            triangles,
            shader: Shader::Dot {
                center: draw.center,
                radius: draw.radius,
                color: draw.color,
                glow_factor: draw.glow_factor,
                shading: draw.shading,
                to_camera,
                scaled_aa_width: draw.anti_alias_width * camera.pixel_size() / draw.radius,
                light_position: camera.light_source_position(),
                camera_position: camera.location(),
            },
        },
        depth_test: draw.depth_test,
    })
}

fn append_clipped_triangle(
    camera: &Camera,
    triangle: &[WorldVertex; 3],
    fixed: f64,
    clip_planes: [[f64; 4]; 4],
    output: &mut Vec<RasterTriangle>,
) {
    let mut polygon = triangle.to_vec();
    for plane in clip_planes {
        if plane[..3].iter().all(|value| *value == 0.0) {
            continue;
        }
        polygon = clip_world_polygon(&polygon, |vertex| {
            let point = vertex.attributes.world;
            point[0] * plane[0] + point[1] * plane[1] + point[2] * plane[2] + plane[3]
        });
        if polygon.len() < 3 {
            return;
        }
    }
    let mut projected: Vec<ProjectedVertex> = polygon
        .into_iter()
        .map(|vertex| ProjectedVertex {
            clip: camera.project(vertex.attributes.world, fixed).clip,
            attributes: vertex.attributes,
        })
        .collect();
    for plane in 0..6 {
        projected = clip_projected_polygon(&projected, |vertex| {
            let [x, y, z, w] = vertex.clip;
            match plane {
                0 => x + w,
                1 => w - x,
                2 => y + w,
                3 => w - y,
                4 => z + w,
                _ => w - z,
            }
        });
        if projected.len() < 3 {
            return;
        }
    }
    let first = projected[0];
    for index in 1..projected.len() - 1 {
        let vertices = [first, projected[index], projected[index + 1]];
        if let Some(triangle) = raster_triangle(vertices, camera.pixel_shape()) {
            output.push(triangle);
        }
    }
}

fn clip_world_polygon(
    input: &[WorldVertex],
    distance: impl Fn(&WorldVertex) -> f64,
) -> Vec<WorldVertex> {
    let mut output = Vec::with_capacity(input.len() + 1);
    let Some(mut previous) = input.last().copied() else {
        return output;
    };
    let mut previous_distance = distance(&previous);
    for &current in input {
        let current_distance = distance(&current);
        let previous_inside = previous_distance >= 0.0;
        let current_inside = current_distance >= 0.0;
        if previous_inside != current_inside {
            let alpha = previous_distance / (previous_distance - current_distance);
            output.push(WorldVertex {
                attributes: previous.attributes.lerp(current.attributes, alpha),
            });
        }
        if current_inside {
            output.push(current);
        }
        previous = current;
        previous_distance = current_distance;
    }
    output
}

fn clip_projected_polygon(
    input: &[ProjectedVertex],
    distance: impl Fn(&ProjectedVertex) -> f64,
) -> Vec<ProjectedVertex> {
    let mut output = Vec::with_capacity(input.len() + 1);
    let Some(mut previous) = input.last().copied() else {
        return output;
    };
    let mut previous_distance = distance(&previous);
    for &current in input {
        let current_distance = distance(&current);
        let previous_inside = previous_distance >= 0.0;
        let current_inside = current_distance >= 0.0;
        if previous_inside != current_inside {
            let alpha = previous_distance / (previous_distance - current_distance);
            output.push(previous.lerp(current, alpha));
        }
        if current_inside {
            output.push(current);
        }
        previous = current;
        previous_distance = current_distance;
    }
    output
}

fn raster_triangle(
    vertices: [ProjectedVertex; 3],
    resolution: (u32, u32),
) -> Option<RasterTriangle> {
    let mut raster = [RasterVertex {
        attributes: vertices[0].attributes,
        screen: [0.0; 2],
        inverse_w: 0.0,
        ndc_z: 0.0,
    }; 3];
    for (slot, vertex) in raster.iter_mut().zip(vertices) {
        let w = vertex.clip[3];
        if w <= 0.0 || !w.is_finite() {
            return None;
        }
        let inverse_w = 1.0 / w;
        let ndc_x = vertex.clip[0] * inverse_w;
        let ndc_y = vertex.clip[1] * inverse_w;
        *slot = RasterVertex {
            attributes: vertex.attributes,
            screen: [
                0.5 * (ndc_x + 1.0) * f64::from(resolution.0),
                0.5 * (1.0 - ndc_y) * f64::from(resolution.1),
            ],
            inverse_w,
            ndc_z: vertex.clip[2] * inverse_w,
        };
    }
    let area = orient(raster[0].screen, raster[1].screen, raster[2].screen);
    if area == 0.0 || !area.is_finite() {
        return None;
    }
    if area < 0.0 {
        raster.swap(1, 2);
    }
    let mut bounds = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for vertex in raster {
        bounds[0] = bounds[0].min(vertex.screen[0]);
        bounds[1] = bounds[1].min(vertex.screen[1]);
        bounds[2] = bounds[2].max(vertex.screen[0]);
        bounds[3] = bounds[3].max(vertex.screen[1]);
    }
    Some(RasterTriangle {
        vertices: raster,
        bounds,
    })
}

fn union_bounds(triangles: &[RasterTriangle]) -> Option<[f64; 4]> {
    let mut output: Option<[f64; 4]> = None;
    for triangle in triangles {
        output = Some(match output {
            None => triangle.bounds,
            Some(bounds) => [
                bounds[0].min(triangle.bounds[0]),
                bounds[1].min(triangle.bounds[1]),
                bounds[2].max(triangle.bounds[2]),
                bounds[3].max(triangle.bounds[3]),
            ],
        });
    }
    output
}

fn bounds_intersect(bounds: Option<[f64; 4]>, rectangle: [u32; 4]) -> bool {
    let Some(bounds) = bounds else {
        return false;
    };
    bounds[2] >= f64::from(rectangle[0])
        && bounds[3] >= f64::from(rectangle[1])
        && bounds[0] <= f64::from(rectangle[2])
        && bounds[1] <= f64::from(rectangle[3])
}

fn orient(a: [f64; 2], b: [f64; 2], point: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (point[1] - a[1]) - (b[1] - a[1]) * (point[0] - a[0])
}

// Screen y grows downward. Under the positive-area winding normalized by
// `raster_triangle`, top edges run left-to-right and left edges run upward.
// Making only those edges inclusive is the half-open top-left rule: two
// adjacent triangles cover a shared sample exactly once.
fn is_top_left_edge(start: [f64; 2], end: [f64; 2]) -> bool {
    let dx = end[0] - start[0];
    let dy = end[1] - start[1];
    dy < 0.0 || (dy == 0.0 && dx > 0.0)
}

fn barycentric(triangle: &RasterTriangle, point: [f64; 2]) -> Option<[f64; 3]> {
    let [a, b, c] = triangle.vertices.map(|vertex| vertex.screen);
    let area = orient(a, b, c);
    let weights = [
        orient(b, c, point),
        orient(c, a, point),
        orient(a, b, point),
    ];
    let edges = [(b, c), (c, a), (a, b)];
    if weights
        .into_iter()
        .zip(edges)
        .any(|(weight, (start, end))| {
            weight < 0.0 || (weight == 0.0 && !is_top_left_edge(start, end))
        })
    {
        return None;
    }
    Some([weights[0] / area, weights[1] / area, weights[2] / area])
}

fn perspective_attributes(
    triangle: &RasterTriangle,
    barycentric: [f64; 3],
) -> Option<(Attributes, f64)> {
    let mut denominator = 0.0;
    let mut corrected = [0.0; 3];
    for index in 0..3 {
        corrected[index] = barycentric[index] * triangle.vertices[index].inverse_w;
        denominator += corrected[index];
    }
    if denominator == 0.0 || !denominator.is_finite() {
        return None;
    }
    for value in &mut corrected {
        *value /= denominator;
    }
    let mut attributes = Attributes {
        world: [0.0; 3],
        normal: [0.0; 3],
        color: [0.0; 4],
        uv: [0.0; 2],
        opacity: 0.0,
    };
    let mut ndc_z = 0.0;
    for index in 0..3 {
        let vertex = triangle.vertices[index];
        let weight = corrected[index];
        for component in 0..3 {
            attributes.world[component] += vertex.attributes.world[component] * weight;
            attributes.normal[component] += vertex.attributes.normal[component] * weight;
        }
        for component in 0..4 {
            attributes.color[component] += vertex.attributes.color[component] * weight;
        }
        for component in 0..2 {
            attributes.uv[component] += vertex.attributes.uv[component] * weight;
        }
        attributes.opacity += vertex.attributes.opacity * weight;
        // Window-space depth is affine in screen barycentrics.
        ndc_z += vertex.ndc_z * barycentric[index];
    }
    Some((attributes, 0.5 * (ndc_z + 1.0)))
}

fn bezier_point(control: [Vec3; 3], t: f64) -> Vec3 {
    let u = 1.0 - t;
    std::array::from_fn(|axis| {
        u * u * control[0][axis] + 2.0 * u * t * control[1][axis] + t * t * control[2][axis]
    })
}

fn bezier_tangent(control: [Vec3; 3], t: f64) -> Vec3 {
    add(
        mul(sub(control[1], control[0]), 2.0 * (1.0 - t)),
        mul(sub(control[2], control[1]), 2.0 * t),
    )
}

fn projected_half_widths(
    camera: &Camera,
    style: &Style,
    flat_normal: Vec3,
    world: [Vec3; 3],
    t: f64,
    s: f64,
) -> [f64; 2] {
    let s = s.clamp(0.0, 1.0) as f32;
    let width = style.stroke_width + (style.stroke_width_end - style.stroke_width) * s;
    projected_half_widths_for(camera, style, flat_normal, world, t, f64::from(width))
}

fn stroke_frame(
    camera: &Camera,
    style: &Style,
    flat_normal: Vec3,
    world: [Vec3; 3],
    t: f64,
) -> Option<(Vec3, Vec3, Vec3)> {
    let point = bezier_point(world, t);
    let tangent = bezier_tangent(world, t);
    let flat = style.flat_stroke || style.is_fixed_in_frame != 0.0;
    let construction_normal = if flat {
        flat_normal
    } else {
        normalize(sub(camera.location(), point)).unwrap_or([0.0, 0.0, 1.0])
    };
    let tangent = if flat {
        tangent
    } else {
        sub(
            tangent,
            mul(construction_normal, dot(tangent, construction_normal)),
        )
    };
    let step = normalize(cross(construction_normal, tangent))?;
    let plane_normal = normalize(cross(tangent, step)).unwrap_or(construction_normal);
    Some((point, step, plane_normal))
}

fn stroke_half_world(camera: &Camera, style: &Style, width_units: f64) -> f64 {
    // Pinned Reference stroke/vert.glsl:
    // 0.01 * width * mix(frame_scale, 1, scale_stroke_with_zoom).
    let zoom = if style.scale_stroke_with_zoom {
        1.0
    } else {
        camera.frame().scale()
    };
    0.5 * width_units * fmn_core::constants::STROKE_WIDTH_CONVERSION * zoom
}

fn projected_half_widths_for(
    camera: &Camera,
    style: &Style,
    flat_normal: Vec3,
    world: [Vec3; 3],
    t: f64,
    width_units: f64,
) -> [f64; 2] {
    if width_units <= 0.0 {
        return [0.0; 2];
    }
    let Some((point, step, _)) = stroke_frame(camera, style, flat_normal, world, t) else {
        return [0.0; 2];
    };
    let half_world = stroke_half_world(camera, style, width_units);
    let Some(center) = camera
        .project(point, style.is_fixed_in_frame)
        .pixel(camera.pixel_shape())
    else {
        return [0.0; 2];
    };
    let plus = camera
        .project(add(point, mul(step, half_world)), style.is_fixed_in_frame)
        .pixel(camera.pixel_shape());
    let minus = camera
        .project(sub(point, mul(step, half_world)), style.is_fixed_in_frame)
        .pixel(camera.pixel_shape());
    [
        plus.map_or(0.0, |pixel| {
            ((pixel[0] - center[0]).powi(2) + (pixel[1] - center[1]).powi(2)).sqrt()
        }),
        minus.map_or(0.0, |pixel| {
            ((pixel[0] - center[0]).powi(2) + (pixel[1] - center[1]).powi(2)).sqrt()
        }),
    ]
}

fn projected_width_toward(
    camera: &Camera,
    style: &Style,
    flat_normal: Vec3,
    position: CurvePosition,
    point: [f64; 2],
    width_units: Option<f64>,
) -> f64 {
    let width_units = width_units.unwrap_or_else(|| {
        let s = position.s.clamp(0.0, 1.0) as f32;
        f64::from(style.stroke_width + (style.stroke_width_end - style.stroke_width) * s)
    });
    if width_units <= 0.0 {
        return 0.0;
    }
    let Some((world, step, _)) =
        stroke_frame(camera, style, flat_normal, position.world, position.t)
    else {
        return 0.0;
    };
    let Some(center) = camera
        .project(world, style.is_fixed_in_frame)
        .pixel(camera.pixel_shape())
    else {
        return 0.0;
    };
    let half_world = stroke_half_world(camera, style, width_units);
    let plus = camera
        .project(add(world, mul(step, half_world)), style.is_fixed_in_frame)
        .pixel(camera.pixel_shape());
    let minus = camera
        .project(sub(world, mul(step, half_world)), style.is_fixed_in_frame)
        .pixel(camera.pixel_shape());
    let widths = [
        plus.map_or(0.0, |pixel| {
            ((pixel[0] - center[0]).powi(2) + (pixel[1] - center[1]).powi(2)).sqrt()
        }),
        minus.map_or(0.0, |pixel| {
            ((pixel[0] - center[0]).powi(2) + (pixel[1] - center[1]).powi(2)).sqrt()
        }),
    ];
    let query = [point[0] - center[0], point[1] - center[1]];
    let plus_direction = plus
        .map(|pixel| [pixel[0] - center[0], pixel[1] - center[1]])
        .or_else(|| minus.map(|pixel| [center[0] - pixel[0], center[1] - pixel[1]]));
    if plus_direction
        .is_some_and(|direction| query[0] * direction[0] + query[1] * direction[1] >= 0.0)
    {
        widths[0]
    } else {
        widths[1]
    }
}

#[derive(Debug, Clone, Copy)]
struct VectorNearest {
    distance: f64,
    s: f64,
    t: f64,
    curve: usize,
    world: Vec3,
    depth: f64,
}

#[derive(Debug, Clone, Copy)]
struct VectorFragment {
    source: LinearRgba,
    depth: f32,
}

#[derive(Debug, Clone, Copy)]
struct VectorSample {
    fill: Option<VectorFragment>,
    stroke: Option<VectorFragment>,
    stroke_behind: bool,
}

fn nearest_on_vector_curve(
    camera: &Camera,
    vector: &CompiledVector,
    curve: usize,
    point: [f64; 2],
) -> VectorNearest {
    let curve_piece = &vector.curves[curve];
    let near = fmn_geom::distance::nearest_on_quadratic(
        curve_piece.screen.p0,
        curve_piece.screen.p1,
        curve_piece.screen.p2,
        [point[0], point[1], 0.0],
    );
    let total = fmn_geom::arclength::quadratic_arc_length(
        curve_piece.world[0],
        curve_piece.world[1],
        curve_piece.world[2],
    );
    let fraction = if total > 0.0 {
        let partial = fmn_geom::bezier::partial_quadratic(&curve_piece.world, 0.0, near.t);
        (fmn_geom::arclength::quadratic_arc_length(partial[0], partial[1], partial[2]) / total)
            .clamp(0.0, 1.0)
    } else {
        0.0
    };
    let s = curve_piece.screen.s0 + (curve_piece.screen.s1 - curve_piece.screen.s0) * fraction;
    let world = bezier_point(curve_piece.world, near.t);
    let depth = camera
        .project(world, vector.style.is_fixed_in_frame)
        .pixel(camera.pixel_shape())
        .map_or(1.0, |pixel| pixel[2]);
    VectorNearest {
        distance: near.distance,
        s,
        t: near.t,
        curve,
        world,
        depth,
    }
}

fn vector_nearest(
    camera: &Camera,
    vector: &CompiledVector,
    point: [f64; 2],
) -> Option<VectorNearest> {
    let mut best: Option<VectorNearest> = None;
    for curve in 0..vector.curves.len() {
        let nearest = nearest_on_vector_curve(camera, vector, curve, point);
        if best.is_some_and(|current| nearest.distance >= current.distance) {
            continue;
        }
        best = Some(nearest);
    }
    best
}

fn vector_stroke_sample(
    camera: &Camera,
    vector: &CompiledVector,
    point: [f64; 2],
) -> Option<(f64, f64, Vec3, f64)> {
    if !vector.draws_stroke {
        return None;
    }
    let mut best: Option<(f64, VectorNearest)> = None;
    for curve_index in 0..vector.curves.len() {
        let nearest = nearest_on_vector_curve(camera, vector, curve_index, point);
        let curve = &vector.curves[curve_index];
        let half_width = projected_width_toward(
            camera,
            &vector.style,
            vector.normal,
            CurvePosition {
                world: curve.world,
                t: nearest.t,
                s: nearest.s,
            },
            point,
            None,
        );
        let excess = nearest.distance - half_width;
        if best.is_none_or(|(current, _)| excess < current) {
            best = Some((excess, nearest));
        }
    }
    let (round_excess, nearest) = best?;
    let curve = &vector.curves[nearest.curve];
    let excess =
        crate::stroke::apply_joins(round_excess, &vector.joins, vector.style.joint_type, point);
    let coverage = aa_coverage(excess, f64::from(vector.style.anti_alias_width));
    let fragment_world = stroke_frame(camera, &vector.style, vector.normal, curve.world, nearest.t)
        .and_then(|(_, _, plane_normal)| {
            let plane = [
                plane_normal[0],
                plane_normal[1],
                plane_normal[2],
                -dot(plane_normal, nearest.world),
            ];
            world_on_vector_plane(camera, point, vector.style.is_fixed_in_frame, plane)
        })
        .unwrap_or(nearest.world);
    let projected = camera.project(fragment_world, vector.style.is_fixed_in_frame);
    if !projected.inside_clip_volume()
        || vector.style.clip_planes.iter().any(|plane| {
            projected
                .user_clip_distance(*plane)
                .is_some_and(|distance| distance < 0.0)
        })
    {
        return None;
    }
    let depth = projected
        .pixel(camera.pixel_shape())
        .map_or(nearest.depth, |pixel| pixel[2]);
    Some((coverage, nearest.s, fragment_world, depth))
}

fn solve_three_by_three(mut matrix: [[f64; 4]; 3]) -> Option<Vec3> {
    for pivot in 0..3 {
        let row = (pivot..3)
            .max_by(|&a, &b| matrix[a][pivot].abs().total_cmp(&matrix[b][pivot].abs()))?;
        matrix.swap(pivot, row);
        let divisor = matrix[pivot][pivot];
        if divisor.abs() <= 1e-14 || !divisor.is_finite() {
            return None;
        }
        for value in &mut matrix[pivot][pivot..] {
            *value /= divisor;
        }
        let pivot_row = matrix[pivot];
        for (target, row) in matrix.iter_mut().enumerate() {
            if target == pivot {
                continue;
            }
            let factor = row[pivot];
            for (value, pivot_value) in row[pivot..].iter_mut().zip(&pivot_row[pivot..]) {
                *value -= factor * pivot_value;
            }
        }
    }
    Some([matrix[0][3], matrix[1][3], matrix[2][3]])
}

fn world_on_vector_plane(
    camera: &Camera,
    pixel: [f64; 2],
    fixed: f64,
    plane: [f64; 4],
) -> Option<Vec3> {
    let ndc_x = 2.0 * pixel[0] / f64::from(camera.pixel_width()) - 1.0;
    let ndc_y = 1.0 - 2.0 * pixel[1] / f64::from(camera.pixel_height());
    let origin = camera.project([0.0; 3], fixed).clip;
    let columns: [[f64; 4]; 3] = std::array::from_fn(|axis| {
        let mut basis = [0.0; 3];
        basis[axis] = 1.0;
        let projected = camera.project(basis, fixed).clip;
        std::array::from_fn(|component| projected[component] - origin[component])
    });
    solve_three_by_three([
        [
            columns[0][0] - ndc_x * columns[0][3],
            columns[1][0] - ndc_x * columns[1][3],
            columns[2][0] - ndc_x * columns[2][3],
            -(origin[0] - ndc_x * origin[3]),
        ],
        [
            columns[0][1] - ndc_y * columns[0][3],
            columns[1][1] - ndc_y * columns[1][3],
            columns[2][1] - ndc_y * columns[2][3],
            -(origin[1] - ndc_y * origin[3]),
        ],
        [plane[0], plane[1], plane[2], -plane[3]],
    ])
}

fn rgba_from_array(value: [f32; 4]) -> LinearRgba {
    LinearRgba {
        r: f64::from(value[0]),
        g: f64::from(value[1]),
        b: f64::from(value[2]),
        a: f64::from(value[3]),
    }
}

fn shade_vector(
    camera: &Camera,
    vector: &CompiledVector,
    pixel: [u32; 2],
    samples: u32,
    sample: [u32; 2],
) -> Option<VectorSample> {
    let point = [
        f64::from(pixel[0]) + (f64::from(sample[0]) + 0.5) / f64::from(samples),
        f64::from(pixel[1]) + (f64::from(sample[1]) + 0.5) / f64::from(samples),
    ];
    let fill_coverage = if vector.draws_fill {
        fill::coverage_at_subcell(
            &vector.fill,
            [0.0; 2],
            pixel[1],
            pixel[0],
            samples,
            sample[0],
            sample[1],
        )
    } else {
        0.0
    };
    let stroke = vector_stroke_sample(camera, vector, point);
    let mut fill_source = None;
    let mut fill_depth = None;
    if fill_coverage > 0.0 {
        let fill_world = vector.fill_plane.and_then(|plane| {
            world_on_vector_plane(camera, point, vector.style.is_fixed_in_frame, plane)
        });
        let parameter = vector
            .field
            .as_ref()
            .zip(fill_world)
            .map_or(0.0, |(field, world)| field.param_at(world));
        let mut color = rgba_from_array(fill::fill_rgba_at(&vector.style, parameter));
        if !fill::fill_is_flat(&vector.style)
            && vector.style.fill_border_width > 0.0
            && let Some(nearest) = vector_nearest(camera, vector, point)
        {
            let curve = &vector.curves[nearest.curve];
            // `border_width_px` is the whole inward band, whereas the stroke
            // construction helper accepts a full centred-stroke width and
            // returns its half-width. Doubling here preserves the same unit
            // conversion without growing the fill silhouette.
            let width = projected_width_toward(
                camera,
                &vector.style,
                vector.normal,
                CurvePosition {
                    world: curve.world,
                    t: nearest.t,
                    s: nearest.s,
                },
                point,
                Some(2.0 * f64::from(vector.style.fill_border_width)),
            );
            let border = fill::border_coverage(
                nearest.distance,
                width,
                f64::from(vector.style.anti_alias_width),
            );
            if border > 0.0 {
                let edge = rgba_from_array(fill::fill_rgba_at(&vector.style, nearest.s));
                color = mix_color(color, edge, border);
            }
        }
        if let Some(plane) = vector.fill_plane
            && let Some(world) = fill_world
        {
            if vector.style.shading != [0.0; 3] {
                color = finalize_color(
                    color,
                    world,
                    [plane[0], plane[1], plane[2]],
                    vector.style.shading,
                    camera.light_source_position(),
                    camera.location(),
                );
            }
            fill_depth = camera
                .project(world, vector.style.is_fixed_in_frame)
                .pixel(camera.pixel_shape())
                .map(|projected| projected[2]);
        }
        color.a *= fill_coverage;
        fill_source = Some(VectorFragment {
            source: color,
            depth: fill_depth.unwrap_or(1.0) as f32,
        });
    }
    let stroke_source = stroke.and_then(|(coverage, s, world, depth)| {
        if coverage <= 0.0 {
            return None;
        }
        let mut color = rgba_from_array(stroke_rgba_at(&vector.style, s));
        if vector.style.shading != [0.0; 3] {
            color = finalize_color(
                color,
                world,
                vector.normal,
                vector.style.shading,
                camera.light_source_position(),
                camera.location(),
            );
        }
        color.a *= coverage;
        Some(VectorFragment {
            source: color,
            depth: depth as f32,
        })
    });
    if fill_source.is_none() && stroke_source.is_none() {
        return None;
    }
    Some(VectorSample {
        fill: fill_source,
        stroke: stroke_source,
        stroke_behind: vector.style.stroke_behind,
    })
}

fn source_over_premul(source: PremulRgba, destination: PremulRgba) -> PremulRgba {
    let remaining = 1.0 - source.a;
    PremulRgba {
        r: source.r + destination.r * remaining,
        g: source.g + destination.g * remaining,
        b: source.b + destination.b * remaining,
        a: source.a + destination.a * remaining,
    }
}

struct TileScratch {
    tile: usize,
    sample_grid: u32,
    samples_per_pixel: usize,
    boundary: Vec<bool>,
    color: Vec<PremulRgba>,
    depth: Vec<f32>,
}

impl TileScratch {
    fn new(tile: usize, sample_grid: u32) -> Self {
        let samples_per_pixel = (sample_grid * sample_grid) as usize;
        let pixels = tile.saturating_mul(tile);
        Self {
            tile,
            sample_grid,
            samples_per_pixel,
            boundary: vec![false; pixels],
            color: vec![PremulRgba::TRANSPARENT; pixels.saturating_mul(samples_per_pixel)],
            depth: vec![1.0; pixels.saturating_mul(samples_per_pixel)],
        }
    }

    fn clear(&mut self, width: usize, height: usize, background: LinearRgba) {
        debug_assert!(width <= self.tile && height <= self.tile);
        let pixels = width * height;
        self.boundary[..pixels].fill(false);
        self.color[..pixels * self.samples_per_pixel].fill(background.premultiply());
        self.depth[..pixels * self.samples_per_pixel].fill(1.0);
    }

    fn mark_triangle_boundary(
        &mut self,
        triangle: &RasterTriangle,
        rectangle: [u32; 4],
        width: usize,
        height: usize,
    ) {
        let [x0, y0, x1, y1] = rectangle;
        let lo_x = triangle.bounds[0].floor().max(f64::from(x0)) as u32;
        let lo_y = triangle.bounds[1].floor().max(f64::from(y0)) as u32;
        let hi_x = triangle.bounds[2].ceil().min(f64::from(x1)) as u32;
        let hi_y = triangle.bounds[3].ceil().min(f64::from(y1)) as u32;
        for y in lo_y..hi_y {
            for x in lo_x..hi_x {
                let corners = [
                    [f64::from(x), f64::from(y)],
                    [f64::from(x + 1), f64::from(y)],
                    [f64::from(x), f64::from(y + 1)],
                    [f64::from(x + 1), f64::from(y + 1)],
                ];
                if !corners
                    .into_iter()
                    .all(|point| barycentric(triangle, point).is_some())
                {
                    let local_x = (x - x0) as usize;
                    let local_y = (y - y0) as usize;
                    if local_x < width && local_y < height {
                        self.boundary[local_y * width + local_x] = true;
                    }
                }
            }
        }
    }

    fn mark_vector_boundaries(
        &mut self,
        camera: &Camera,
        vector: &CompiledVector,
        rectangle: [u32; 4],
        width: usize,
        height: usize,
    ) {
        let [x0, y0, x1, y1] = rectangle;
        for y in y0..y1 {
            for x in x0..x1 {
                let fill = if vector.draws_fill {
                    fill::coverage_at_cell::<f64>(&vector.fill, [0.0; 2], y, x)
                } else {
                    0.0
                };
                let fill_edge = vector.draws_fill
                    && fill::boundary_crossings_at_cell(&vector.fill, [0.0; 2], y, x) > 0;
                let center =
                    vector_stroke_sample(camera, vector, [f64::from(x) + 0.5, f64::from(y) + 0.5])
                        .map_or(0.0, |sample| sample.0);
                let mut stroke_min = center;
                let mut stroke_max = center;
                for sample_y in 0..self.sample_grid {
                    for sample_x in 0..self.sample_grid {
                        let dy = (f64::from(sample_y) + 0.5) / f64::from(self.sample_grid);
                        let dx = (f64::from(sample_x) + 0.5) / f64::from(self.sample_grid);
                        let coverage = vector_stroke_sample(
                            camera,
                            vector,
                            [f64::from(x) + dx, f64::from(y) + dy],
                        )
                        .map_or(0.0, |sample| sample.0);
                        stroke_min = stroke_min.min(coverage);
                        stroke_max = stroke_max.max(coverage);
                    }
                }
                let stroke_edge =
                    stroke_max > 0.0 && (stroke_min < 1.0 || stroke_max - stroke_min > 1e-12);
                if (fill > 0.0 && fill < 1.0) || fill_edge || stroke_edge {
                    let local_x = (x - x0) as usize;
                    let local_y = (y - y0) as usize;
                    if local_x < width && local_y < height {
                        self.boundary[local_y * width + local_x] = true;
                    }
                }
            }
        }
    }

    fn raster_triangle(
        &mut self,
        triangle: &RasterTriangle,
        shader: Shader<'_>,
        depth_test: bool,
        rectangle: [u32; 4],
        width: usize,
        height: usize,
    ) {
        let [x0, y0, x1, y1] = rectangle;
        let lo_x = triangle.bounds[0].floor().max(f64::from(x0)) as u32;
        let lo_y = triangle.bounds[1].floor().max(f64::from(y0)) as u32;
        let hi_x = triangle.bounds[2].ceil().min(f64::from(x1)) as u32;
        let hi_y = triangle.bounds[3].ceil().min(f64::from(y1)) as u32;
        for y in lo_y..hi_y {
            for x in lo_x..hi_x {
                let local_x = (x - x0) as usize;
                let local_y = (y - y0) as usize;
                if local_x >= width || local_y >= height {
                    continue;
                }
                let pixel = local_y * width + local_x;
                let samples = if self.boundary[pixel] {
                    self.sample_grid
                } else {
                    1
                };
                for sample_y in 0..samples {
                    for sample_x in 0..samples {
                        let point = [
                            f64::from(x) + (f64::from(sample_x) + 0.5) / f64::from(samples),
                            f64::from(y) + (f64::from(sample_y) + 0.5) / f64::from(samples),
                        ];
                        let Some(weights) = barycentric(triangle, point) else {
                            continue;
                        };
                        let Some((attributes, depth)) = perspective_attributes(triangle, weights)
                        else {
                            continue;
                        };
                        let sample = if samples == 1 {
                            0
                        } else {
                            (sample_y * self.sample_grid + sample_x) as usize
                        };
                        let slot = pixel * self.samples_per_pixel + sample;
                        let depth = depth as f32;
                        if depth_test && depth >= self.depth[slot] {
                            continue;
                        }
                        let Some(source) = shade(shader, attributes) else {
                            continue;
                        };
                        self.color[slot] = source_over(source, self.color[slot]);
                        if depth_test {
                            self.depth[slot] = depth;
                        }
                    }
                }
            }
        }
    }

    fn raster_vector(
        &mut self,
        camera: &Camera,
        vector: &CompiledVector,
        depth_test: bool,
        rectangle: [u32; 4],
        width: usize,
        height: usize,
    ) {
        let [x0, y0, x1, y1] = rectangle;
        for y in y0..y1 {
            for x in x0..x1 {
                let local_x = (x - x0) as usize;
                let local_y = (y - y0) as usize;
                if local_x >= width || local_y >= height {
                    continue;
                }
                let pixel = local_y * width + local_x;
                let samples = if self.boundary[pixel] {
                    self.sample_grid
                } else {
                    1
                };
                for sample_y in 0..samples {
                    for sample_x in 0..samples {
                        let Some(vector_sample) =
                            shade_vector(camera, vector, [x, y], samples, [sample_x, sample_y])
                        else {
                            continue;
                        };
                        let sample = if samples == 1 {
                            0
                        } else {
                            (sample_y * self.sample_grid + sample_x) as usize
                        };
                        let slot = pixel * self.samples_per_pixel + sample;
                        let fragments = if vector_sample.stroke_behind {
                            [vector_sample.stroke, vector_sample.fill]
                        } else {
                            [vector_sample.fill, vector_sample.stroke]
                        };
                        if !depth_test {
                            // Preserve the certified association used before
                            // fill/stroke acquired distinct depth values:
                            // assemble this vector command over transparent,
                            // then composite the command once into the painter
                            // sequence. Source-over is associative over the
                            // reals, but changing the f64 grouping can move f16
                            // output bits for no semantic gain.
                            let mut source = PremulRgba::TRANSPARENT;
                            for fragment in fragments.into_iter().flatten() {
                                source = source_over(fragment.source, source);
                            }
                            self.color[slot] = source_over_premul(source, self.color[slot]);
                            continue;
                        }
                        for fragment in fragments.into_iter().flatten() {
                            if fragment.depth >= self.depth[slot] {
                                continue;
                            }
                            self.color[slot] = source_over(fragment.source, self.color[slot]);
                            self.depth[slot] = fragment.depth;
                        }
                    }
                }
            }
        }
    }

    fn resolve(&self, x: usize, y: usize, width: usize) -> PremulRgba {
        let pixel = y * width + x;
        if !self.boundary[pixel] || self.samples_per_pixel == 1 {
            return self.color[pixel * self.samples_per_pixel];
        }
        let start = pixel * self.samples_per_pixel;
        let mut sum = PremulRgba::TRANSPARENT;
        for sample in &self.color[start..start + self.samples_per_pixel] {
            sum.r += sample.r;
            sum.g += sample.g;
            sum.b += sample.b;
            sum.a += sample.a;
        }
        let inverse = 1.0 / self.samples_per_pixel as f64;
        PremulRgba {
            r: sum.r * inverse,
            g: sum.g * inverse,
            b: sum.b * inverse,
            a: sum.a * inverse,
        }
    }
}

fn shade(shader: Shader<'_>, attributes: Attributes) -> Option<LinearRgba> {
    match shader {
        Shader::Gouraud => {
            let mut color = array_rgba(attributes.color);
            color.a *= attributes.opacity.clamp(0.0, 1.0);
            Some(color)
        }
        Shader::Texture {
            material,
            shading,
            light_position,
            camera_position,
        } => {
            let mut color = material.light.sample(attributes.uv, material.sampler);
            // The Reference interpolates already-unit vertex normals and uses
            // the resulting varying directly. Renormalizing here changes both
            // its light/dark texture transition and fragment lighting.
            let normal = attributes.normal;
            if let Some(dark) = material.dark {
                let dark_color = dark.sample(attributes.uv, material.sampler);
                let to_light = normalize(sub(light_position, attributes.world)).unwrap_or([0.0; 3]);
                let alpha = smoothstep(-DARK_SHIFT, DARK_SHIFT, dot(to_light, normal));
                color = mix_color(dark_color, color, alpha);
            }
            if material.discard_transparent && color.a == 0.0 {
                return None;
            }
            color = finalize_color(
                color,
                attributes.world,
                normal,
                shading,
                light_position,
                camera_position,
            );
            if material.discard_transparent {
                // TexturedSurface overrides sampled alpha with its vertex
                // opacity after the zero-alpha discard.
                color.a = attributes.opacity.clamp(0.0, 1.0);
            } else {
                // ImageMobject multiplies sampled alpha.
                color.a *= attributes.opacity.clamp(0.0, 1.0);
            }
            Some(color)
        }
        Shader::Dot {
            center,
            radius,
            mut color,
            glow_factor,
            shading,
            to_camera,
            scaled_aa_width,
            light_position,
            camera_position,
        } => {
            let r =
                (attributes.uv[0] * attributes.uv[0] + attributes.uv[1] * attributes.uv[1]).sqrt();
            if r > 1.0 {
                return None;
            }
            color.a *= true_dot_alpha(r, scaled_aa_width, glow_factor);
            if shading != [0.0; 3] {
                let point = add(
                    attributes.world,
                    mul(to_camera, radius * (1.0 - r * r).max(0.0).sqrt()),
                );
                let normal = normalize(sub(point, center)).unwrap_or(to_camera);
                color = finalize_color(
                    color,
                    point,
                    normal,
                    shading,
                    light_position,
                    camera_position,
                );
            }
            Some(color)
        }
    }
}

fn mix_color(a: LinearRgba, b: LinearRgba, alpha: f64) -> LinearRgba {
    let mix = |x: f64, y: f64| x + (y - x) * alpha;
    LinearRgba {
        r: mix(a.r, b.r),
        g: mix(a.g, b.g),
        b: mix(a.b, b.b),
        a: mix(a.a, b.a),
    }
}

fn source_over(source: LinearRgba, destination: PremulRgba) -> PremulRgba {
    let source = source.premultiply();
    let remaining = 1.0 - source.a;
    PremulRgba {
        r: source.r + destination.r * remaining,
        g: source.g + destination.g * remaining,
        b: source.b + destination.b * remaining,
        a: source.a + destination.a * remaining,
    }
}

fn write_pixel(value: PremulRgba, output: &mut [u8]) {
    let straight = if value.a > 0.0 {
        [
            value.r / value.a,
            value.g / value.a,
            value.b / value.a,
            value.a,
        ]
    } else {
        [0.0; 4]
    };
    for (component, bytes) in straight.into_iter().zip(output.as_chunks_mut::<2>().0) {
        bytes.copy_from_slice(&fmn_frame::half::f16_from_f32(component as f32).to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_core::color::Srgb;
    use fmn_frame::half::f16_to_f64;
    use fmn_hash::sha256;
    use fmn_mobject::{JointType, Mobject, RecordBuffer, RecordSchema, Stage, Uniforms};

    fn color(hex: &str, alpha: f64) -> LinearRgba {
        Srgb::from_hex(hex).expect("hex").to_linear(alpha)
    }

    fn vertex(point: Vec3, normal: Vec3, color: LinearRgba) -> SurfaceVertex {
        SurfaceVertex::colored(point, normal, color)
    }

    fn full_triangle(z: f64, color: LinearRgba) -> SurfaceMesh {
        SurfaceMesh::new(
            vec![
                vertex([-20.0, -20.0, z], [0.0, 0.0, 1.0], color),
                vertex([20.0, -20.0, z], [0.0, 0.0, 1.0], color),
                vertex([0.0, 20.0, z], [0.0, 0.0, 1.0], color),
            ],
            vec![0, 1, 2],
        )
        .expect("mesh")
    }

    fn camera() -> Camera {
        Camera::new(crate::camera::CameraConfig {
            resolution: (32, 24),
            samples: 2,
            background: color("#000000", 1.0),
            ..crate::camera::CameraConfig::default()
        })
        .expect("camera")
    }

    fn pixel(frame: &FrameBuffer, x: u32, y: u32) -> [f64; 4] {
        let stride = frame.layout().stride(0);
        let offset = y as usize * stride + x as usize * 8;
        let bytes = &frame.plane(0)[offset..offset + 8];
        std::array::from_fn(|index| {
            f16_to_f64(u16::from_le_bytes([bytes[index * 2], bytes[index * 2 + 1]]))
        })
    }

    fn compiled_vector<'a>(job: &'a ThreeDJob<'_>) -> &'a CompiledVector {
        job.draws
            .first()
            .and_then(|draw| match &draw.primitive {
                CompiledPrimitive::Vector(vector) => Some(vector.as_ref()),
                CompiledPrimitive::Triangles { .. } => None,
            })
            .expect("fixture must compile as a retained vector")
    }

    fn path_points(corners: &[Vec3], closed: bool) -> Vec<Vec3> {
        let mut points = vec![corners[0]];
        let end = corners.len() + usize::from(closed);
        for index in 1..end {
            let next = corners[index % corners.len()];
            let previous = points[points.len() - 1];
            points.push([
                0.5 * (previous[0] + next[0]),
                0.5 * (previous[1] + next[1]),
                0.5 * (previous[2] + next[2]),
            ]);
            points.push(next);
        }
        points
    }

    fn vector_plan(
        points: &[Vec3],
        fill: [f32; 4],
        stroke: [f32; 4],
        stroke_width: f32,
        camera_revision: u64,
        configure: impl FnOnce(&mut Uniforms),
    ) -> RenderPlan {
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len());
        for (index, point) in points.iter().enumerate() {
            buffer.write(
                index,
                "point",
                &[point[0] as f32, point[1] as f32, point[2] as f32],
            );
            buffer.write(index, "fill_rgba", &fill);
            buffer.write(index, "stroke_rgba", &stroke);
            buffer.write(index, "stroke_width", &[stroke_width]);
            buffer.write(index, "fill_border_width", &[0.0]);
        }
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_buffer(buffer));
        configure(stage.uniforms_mut(mob).expect("live mobject"));
        stage.add_to_scene(mob).expect("rooted");
        let mut plan = RenderPlan::new();
        plan.sync(&stage, camera_revision);
        plan
    }

    fn gradient_vector_plan(
        points: &[Vec3],
        fill_start: [f32; 4],
        fill_end: [f32; 4],
        camera_revision: u64,
        configure: impl FnOnce(&mut Uniforms),
    ) -> RenderPlan {
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len());
        for (index, point) in points.iter().enumerate() {
            buffer.write(
                index,
                "point",
                &[point[0] as f32, point[1] as f32, point[2] as f32],
            );
            let fill = if index + 1 == points.len() {
                fill_end
            } else {
                fill_start
            };
            buffer.write(index, "fill_rgba", &fill);
            buffer.write(index, "stroke_rgba", &[0.0; 4]);
            buffer.write(index, "stroke_width", &[0.0]);
            buffer.write(index, "fill_border_width", &[0.0]);
        }
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_buffer(buffer));
        configure(stage.uniforms_mut(mob).expect("live mobject"));
        stage.add_to_scene(mob).expect("rooted");
        let mut plan = RenderPlan::new();
        plan.sync(&stage, camera_revision);
        plan
    }

    #[test]
    fn uv_grid_indices_match_the_reference_pattern() {
        let vertices = (0..6)
            .map(|index| {
                vertex(
                    [f64::from(index), 0.0, 0.0],
                    [0.0, 0.0, 1.0],
                    color("#FFFFFF", 1.0),
                )
            })
            .collect();
        let mesh = SurfaceMesh::from_uv_grid(vertices, (3, 2)).expect("grid");
        assert_eq!(mesh.indices(), &[0, 2, 1, 1, 2, 3, 2, 4, 3, 3, 4, 5]);
        assert_eq!(mesh.resolution(), Some((3, 2)));
    }

    #[test]
    fn invalid_mesh_and_dot_inputs_fail_closed() {
        let white = color("#FFFFFF", 1.0);
        let vertices = vec![
            vertex([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], white),
            vertex([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], white),
            vertex([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], white),
        ];
        assert_eq!(
            SurfaceMesh::new(vertices.clone(), vec![0, 1]),
            Err(ThreeDError::IncompleteTriangle)
        );
        assert_eq!(
            SurfaceMesh::new(vertices.clone(), vec![0, 1, 3]),
            Err(ThreeDError::IndexOutOfBounds)
        );
        assert_eq!(
            SurfaceMesh::from_uv_grid(vertices, (2, 2)),
            Err(ThreeDError::ResolutionMismatch)
        );

        let camera = camera();
        let invalid_radius = [ThreeDDraw::TrueDot(TrueDotDraw::new([0.0; 3], 0.0, white))];
        assert_eq!(
            ThreeDJob::new(&camera, &invalid_radius, Tiling::default())
                .expect_err("zero-radius dot must be rejected"),
            ThreeDError::InvalidRadius
        );
        let mut dot = TrueDotDraw::new([0.0; 3], 1.0, white);
        dot.anti_alias_width = -1.0;
        assert_eq!(
            ThreeDJob::new(&camera, &[ThreeDDraw::TrueDot(dot)], Tiling::default())
                .expect_err("negative AA width must be rejected"),
            ThreeDError::InvalidAntiAliasWidth
        );
    }

    #[test]
    fn kept_lighting_formula_brightens_and_shadows_without_dark_shift() {
        let base = Srgb {
            r: 0.2,
            g: 0.2,
            b: 0.2,
        }
        .to_linear(0.75);
        let bright = finalize_color(
            base,
            [0.0; 3],
            [0.0, 0.0, 1.0],
            [0.3, 0.2, 0.4],
            [0.0, 0.0, 10.0],
            [0.0, 0.0, 10.0],
        );
        assert!((fmn_frame::transfer::srgb_encode(bright.r) - 0.6).abs() < 1e-12);
        assert_eq!(bright.a, 0.75);

        let shadow = finalize_color(
            base,
            [0.0; 3],
            [0.0, 0.0, 1.0],
            [0.3, 0.0, 0.4],
            [0.0, 0.0, -10.0],
            [0.0, 0.0, 10.0],
        );
        assert!((fmn_frame::transfer::srgb_encode(shadow.r) - 0.12).abs() < 1e-12);
        assert_eq!(DARK_SHIFT, 0.2);
    }

    #[test]
    fn depth_test_is_opt_in_inside_painter_order() {
        let camera = camera();
        let near = full_triangle(2.0, color("#0000FF", 1.0));
        let far = full_triangle(-2.0, color("#FF0000", 1.0));
        let overlay = full_triangle(0.0, color("#00FF00", 0.5));
        let mut near_draw = SurfaceDraw::new(&near);
        near_draw.shading = [0.0; 3];
        let mut far_draw = SurfaceDraw::new(&far);
        far_draw.shading = [0.0; 3];
        let mut overlay_draw = SurfaceDraw::new(&overlay);
        overlay_draw.shading = [0.0; 3];
        overlay_draw.depth_test = false;
        let draws = [
            ThreeDDraw::Surface(near_draw),
            ThreeDDraw::Surface(far_draw),
            ThreeDDraw::Surface(overlay_draw),
        ];
        let frame = ThreeDJob::new(&camera, &draws, Tiling::default())
            .expect("job")
            .render(1)
            .expect("frame");
        let center = pixel(&frame, 16, 12);
        assert!(center[1] > center[0], "painter overlay must remain on top");
        assert!(center[2] > center[0], "far red must fail against near blue");
    }

    #[test]
    fn tilted_flat_and_camera_facing_strokes_have_distinct_projected_widths() {
        let camera = camera();
        // The curve lies in the x-z plane and is almost x-tangent at its
        // midpoint. Its in-plane width direction is therefore almost pure
        // depth (strongly foreshortened), while the billboard direction is y.
        let world = [[-2.0, 0.0, -0.1], [0.0, 0.0, 1.0], [2.0, 0.0, 0.1]];
        let base = Style {
            stroke_width: 24.0,
            stroke_width_end: 24.0,
            ..Style::default()
        };
        let normal = unit_normal(world[0], world[1], world[2]);
        let billboard = projected_half_widths(&camera, &base, normal, world, 0.5, 0.5);
        let flat = projected_half_widths(
            &camera,
            &Style {
                flat_stroke: true,
                ..base
            },
            normal,
            world,
            0.5,
            0.5,
        );
        let billboard_mean = 0.5 * (billboard[0] + billboard[1]);
        let flat_mean = 0.5 * (flat[0] + flat[1]);
        assert!(
            (billboard_mean - flat_mean).abs() > 0.1,
            "tilted world-plane width must foreshorten: billboard={billboard:?}, flat={flat:?}"
        );
    }

    #[test]
    fn perspective_stroke_minimizes_signed_excess_before_choosing_a_curve() {
        let camera = camera();
        let origin = camera
            .project([0.0; 3], 1.0)
            .pixel(camera.pixel_shape())
            .expect("fixed origin");
        let x_basis = camera
            .project([1.0, 0.0, 0.0], 1.0)
            .pixel(camera.pixel_shape())
            .expect("fixed x basis");
        let y_basis = camera
            .project([0.0, 1.0, 0.0], 1.0)
            .pixel(camera.pixel_shape())
            .expect("fixed y basis");
        let world_at = |pixel: [f64; 2]| {
            [
                (pixel[0] - origin[0]) / (x_basis[0] - origin[0]),
                (pixel[1] - origin[1]) / (y_basis[1] - origin[1]),
                0.0,
            ]
        };
        let curve_at = |y: f64, s: f64| {
            let world = [world_at([4.0, y]), world_at([16.0, y]), world_at([28.0, y])];
            ProjectedCurvePiece {
                screen: Segment {
                    p0: [4.0, y, 0.0],
                    p1: [16.0, y, 0.0],
                    p2: [28.0, y, 0.0],
                    s0: s,
                    s1: s,
                },
                world,
            }
        };
        let vector = CompiledVector {
            fill: Vec::new(),
            curves: vec![curve_at(10.5, 0.0), curve_at(11.5, 1.0)],
            joins: Vec::new(),
            field: None,
            style: Style {
                stroke_width: 1.0,
                stroke_width_end: 400.0,
                anti_alias_width: 0.01,
                is_fixed_in_frame: 1.0,
                flat_stroke: true,
                scale_stroke_with_zoom: true,
                ..Style::default()
            },
            normal: [0.0, 0.0, 1.0],
            fill_plane: None,
            draws_fill: false,
            draws_stroke: true,
        };
        let query = [16.0, 10.5];
        assert_eq!(
            vector_nearest(&camera, &vector, query)
                .expect("two curves")
                .s,
            0.0,
            "the narrow curve is geometrically nearest"
        );
        let (coverage, s, _, _) =
            vector_stroke_sample(&camera, &vector, query).expect("wide tube covers query");
        assert!(
            s > 0.99,
            "the farther wide curve must win distance-minus-width: s={s}"
        );
        assert!(coverage > 0.99);
    }

    #[test]
    fn flat_straight_edges_use_the_whole_paths_unit_normal_and_fragment_depth() {
        let camera = camera();
        // z = x/2 + y/4. Every individual edge is straight, so deriving a
        // normal from one quadratic would choose an unrelated z-axis fallback;
        // the VMobject normal is the oriented area normal of the whole path.
        let tilted = path_points(
            &[
                [-2.0, -2.0, -1.5],
                [2.0, -2.0, 0.5],
                [2.0, 2.0, 1.5],
                [-2.0, 2.0, -0.5],
            ],
            true,
        );
        let plan = vector_plan(
            &tilted,
            [0.0; 4],
            [1.0; 4],
            100.0,
            camera.revision(),
            |uniforms| {
                uniforms.flat_stroke = true;
                uniforms.depth_test = true;
            },
        );
        let job = ThreeDJob::new(
            &camera,
            &[ThreeDDraw::Vector(VectorDraw::new(&plan, 0))],
            Tiling::default(),
        )
        .expect("tilted flat-stroke job");
        let vector = compiled_vector(&job);
        let expected = normalize([-8.0, -4.0, 16.0]).expect("nonzero plane normal");
        assert!(
            dot(vector.normal, expected) > 1.0 - 1e-12,
            "flat construction must use the path area normal: {:?}",
            vector.normal
        );
        let curve = &vector.curves[0];
        let per_curve_fallback = unit_normal(curve.world[0], curve.world[1], curve.world[2]);
        assert!(
            dot(vector.normal, per_curve_fallback) < 0.999,
            "fixture must distinguish the path normal from a straight-edge fallback"
        );

        let (center_world, step, _) =
            stroke_frame(&camera, &vector.style, vector.normal, curve.world, 0.5)
                .expect("nondegenerate stroke frame");
        let width = f64::from(
            vector.style.stroke_width
                + (vector.style.stroke_width_end - vector.style.stroke_width) * 0.5,
        );
        let query_world = add(
            center_world,
            mul(step, 0.5 * stroke_half_world(&camera, &vector.style, width)),
        );
        let query = camera
            .project(query_world, vector.style.is_fixed_in_frame)
            .pixel(camera.pixel_shape())
            .expect("offset stroke point is visible");
        let (coverage, _, sampled_world, sampled_depth) =
            vector_stroke_sample(&camera, vector, [query[0], query[1]])
                .expect("query lies inside the stroke");
        assert!(coverage > 0.5);
        let round_trip = camera
            .project(sampled_world, vector.style.is_fixed_in_frame)
            .pixel(camera.pixel_shape())
            .expect("sampled stroke fragment is visible");
        assert!((round_trip[0] - query[0]).abs() < 1e-10);
        assert!((round_trip[1] - query[1]).abs() < 1e-10);
        let center_depth = camera
            .project(center_world, vector.style.is_fixed_in_frame)
            .pixel(camera.pixel_shape())
            .expect("curve center is visible")[2];
        assert!(
            (sampled_depth - center_depth).abs() > 1e-6,
            "flat stroke depth must come from the offset fragment, not its curve center"
        );
    }

    #[test]
    fn vector_planarity_is_invariant_under_large_translation() {
        let translated = Segment {
            p0: [1.0e9, -1.0e9, 1.0e9],
            p1: [1.0e9 + 1.0, -1.0e9, 1.0e9],
            p2: [1.0e9 + 2.0, -1.0e9, 1.0e9 + 1.0e-5],
            s0: 0.0,
            s1: 1.0,
        };
        assert!(
            !vector_is_planar(&[translated], [0.0, 0.0, 1.0]),
            "world-space translation must not enlarge the local planarity tolerance"
        );
    }

    #[test]
    fn fill_and_stroke_depth_partition_as_distinct_fragments() {
        let camera = camera();
        let tilted = path_points(
            &[
                [-2.0, -2.0, -1.5],
                [2.0, -2.0, 0.5],
                [2.0, 2.0, 1.5],
                [-2.0, 2.0, -0.5],
            ],
            true,
        );
        let plan = vector_plan(
            &tilted,
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            200.0,
            camera.revision(),
            |uniforms| uniforms.depth_test = true,
        );
        let job = ThreeDJob::new(
            &camera,
            &[ThreeDDraw::Vector(VectorDraw::new(&plan, 0))],
            Tiling::default(),
        )
        .expect("tilted fill-and-stroke job");
        let vector = compiled_vector(&job);
        let (pixel, sample) = (0..camera.pixel_height())
            .flat_map(|y| (0..camera.pixel_width()).map(move |x| [x, y]))
            .find_map(|pixel| {
                let sample = shade_vector(&camera, vector, pixel, 1, [0, 0])?;
                let (fill, stroke) = (sample.fill?, sample.stroke?);
                ((fill.depth - stroke.depth).abs() > 1e-6).then_some((pixel, sample))
            })
            .expect("fixture must overlap fill and stroke at distinct depths");
        let fill = sample.fill.expect("candidate has fill");
        let stroke = sample.stroke.expect("candidate has stroke");
        let threshold = 0.5 * (fill.depth + stroke.depth);
        let nearer = if fill.depth < stroke.depth {
            fill
        } else {
            stroke
        };

        let background = color("#000000", 1.0);
        let mut scratch = TileScratch::new(1, 1);
        scratch.clear(1, 1, background);
        scratch.depth[0] = threshold;
        scratch.raster_vector(
            &camera,
            vector,
            true,
            [pixel[0], pixel[1], pixel[0] + 1, pixel[1] + 1],
            1,
            1,
        );
        assert_eq!(
            scratch.color[0],
            source_over(nearer.source, background.premultiply()),
            "the farther internal pass must not hitchhike on the nearer pass's depth"
        );
    }

    #[test]
    fn four_by_four_vector_classifier_probes_the_raster_sample_lattice() {
        let camera = Camera::new(crate::camera::CameraConfig {
            resolution: (32, 24),
            samples: 4,
            background: color("#000000", 1.0),
            ..crate::camera::CameraConfig::default()
        })
        .expect("four-by-four camera");
        let screen_y_at = |world_y| {
            camera
                .project([0.0, world_y, 0.0], 1.0)
                .pixel(camera.pixel_shape())
                .expect("fixed-frame point is visible")[1]
        };
        let screen_zero = screen_y_at(0.0);
        let screen_one = screen_y_at(1.0);
        let target_y = 10.125;
        let world_y = (target_y - screen_zero) / (screen_one - screen_zero);
        let line = path_points(&[[-5.0, world_y, 0.0], [5.0, world_y, 0.0]], false);
        let plan = vector_plan(
            &line,
            [0.0; 4],
            [1.0; 4],
            1.0,
            camera.revision(),
            |uniforms| {
                uniforms.is_fixed_in_frame = 1.0;
                uniforms.flat_stroke = true;
                uniforms.scale_stroke_with_zoom = true;
                uniforms.anti_alias_width = 0.001;
            },
        );
        let job = ThreeDJob::new(
            &camera,
            &[ThreeDDraw::Vector(VectorDraw::new(&plan, 0))],
            Tiling::default(),
        )
        .expect("thin fixed-frame stroke");
        let vector = compiled_vector(&job);
        let coverage_at = |dx, dy| {
            vector_stroke_sample(&camera, vector, [16.0 + dx, 10.0 + dy])
                .map_or(0.0, |sample| sample.0)
        };
        assert_eq!(coverage_at(0.5, 0.5), 0.0);
        assert!(
            (0..3).all(|y| (0..3).all(|x| {
                coverage_at((f64::from(x) + 0.5) / 3.0, (f64::from(y) + 0.5) / 3.0) == 0.0
            })),
            "the former fixed three-by-three classifier must miss this edge"
        );
        assert!(
            (0..4).any(|y| (0..4).any(|x| {
                coverage_at((f64::from(x) + 0.5) / 4.0, (f64::from(y) + 0.5) / 4.0) > 0.0
            })),
            "the actual four-by-four raster lattice must observe this edge"
        );

        let mut scratch = TileScratch::new(1, 4);
        scratch.mark_vector_boundaries(&camera, vector, [16, 10, 17, 11], 1, 1);
        assert!(
            scratch.boundary[0],
            "the classifier must promote every pixel whose real sample lattice sees an edge"
        );
    }

    #[test]
    fn perspective_vectors_keep_bevel_and_miter_joint_overrides() {
        let camera = camera();
        let corner = path_points(
            &[[-2.5, -1.5, -0.5], [0.0, 0.0, 0.5], [2.0, -2.0, 1.0]],
            false,
        );
        let plan_for = |joint_type| {
            vector_plan(
                &corner,
                [0.0; 4],
                [1.0; 4],
                400.0,
                camera.revision(),
                |uniforms| uniforms.joint_type = joint_type,
            )
        };
        let bevel_plan = plan_for(JointType::Bevel);
        let miter_plan = plan_for(JointType::Miter);
        let bevel = ThreeDJob::new(
            &camera,
            &[ThreeDDraw::Vector(VectorDraw::new(&bevel_plan, 0))],
            Tiling::default(),
        )
        .expect("bevel job")
        .render(1)
        .expect("bevel frame");
        let miter = ThreeDJob::new(
            &camera,
            &[ThreeDDraw::Vector(VectorDraw::new(&miter_plan, 0))],
            Tiling::default(),
        )
        .expect("miter job")
        .render(1)
        .expect("miter frame");
        assert_ne!(
            sha256(bevel.as_bytes()),
            sha256(miter.as_bytes()),
            "perspective compilation must retain the joint override"
        );
    }

    #[test]
    fn retained_vectors_surfaces_and_true_dots_share_one_painter_sequence() {
        let camera = camera();
        let square = path_points(
            &[
                [-3.0, -3.0, 0.0],
                [3.0, -3.0, 0.0],
                [3.0, 3.0, 0.0],
                [-3.0, 3.0, 0.0],
            ],
            true,
        );
        let plan = vector_plan(
            &square,
            [0.0, 1.0, 0.0, 0.5],
            [0.0; 4],
            0.0,
            camera.revision(),
            |_| {},
        );
        let surface = full_triangle(-1.0, color("#FF0000", 1.0));
        let mut surface_draw = SurfaceDraw::new(&surface);
        surface_draw.shading = [0.0; 3];
        surface_draw.depth_test = false;
        let dot = TrueDotDraw::new([0.0, 0.0, 1.0], 3.0, color("#0000FF", 0.5));

        let ordered = [
            ThreeDDraw::Surface(surface_draw),
            ThreeDDraw::Vector(VectorDraw::new(&plan, 0)),
            ThreeDDraw::TrueDot(dot),
        ];
        let reordered = [
            ThreeDDraw::Vector(VectorDraw::new(&plan, 0)),
            ThreeDDraw::Surface(surface_draw),
            ThreeDDraw::TrueDot(dot),
        ];
        let a = ThreeDJob::new(&camera, &ordered, Tiling::default())
            .expect("mixed job")
            .render(1)
            .expect("frame");
        let b = ThreeDJob::new(&camera, &reordered, Tiling::default())
            .expect("mixed job")
            .render(1)
            .expect("frame");
        let a = pixel(&a, 16, 12);
        let b = pixel(&b, 16, 12);
        assert!(
            a[1] > b[1] + 0.15,
            "vector must remain above the first surface"
        );
        assert!(a[0] + 0.15 < b[0], "later surface must cover the vector");
        assert!(
            a[2] > 0.4 && b[2] > 0.4,
            "the final true dot stays above both: ordered={a:?}, reordered={b:?}"
        );
    }

    #[test]
    fn vector_depth_is_per_fragment_and_never_reorders_a_later_overlay() {
        let camera = camera();
        let square = path_points(
            &[
                [-3.0, -3.0, 2.0],
                [3.0, -3.0, 2.0],
                [3.0, 3.0, 2.0],
                [-3.0, 3.0, 2.0],
            ],
            true,
        );
        let plan = vector_plan(
            &square,
            [0.0, 0.0, 1.0, 1.0],
            [0.0; 4],
            0.0,
            camera.revision(),
            |uniforms| uniforms.depth_test = true,
        );
        let far = full_triangle(-2.0, color("#FF0000", 1.0));
        let mut far_draw = SurfaceDraw::new(&far);
        far_draw.shading = [0.0; 3];
        let overlay = TrueDotDraw::new([0.0, 0.0, 0.0], 3.0, color("#00FF00", 0.5));
        let draws = [
            ThreeDDraw::Vector(VectorDraw::new(&plan, 0)),
            ThreeDDraw::Surface(far_draw),
            ThreeDDraw::TrueDot(overlay),
        ];
        let frame = ThreeDJob::new(&camera, &draws, Tiling::default())
            .expect("mixed depth job")
            .render(1)
            .expect("frame");
        let center = pixel(&frame, 16, 12);
        assert!(
            center[0] < 0.05,
            "far red surface must fail the vector depth"
        );
        assert!(
            center[1] > 0.4,
            "non-depth overlay still composites later: {center:?}"
        );
        assert!(center[2] > 0.4, "near vector remains visible");
    }

    #[test]
    fn user_clip_plane_cuts_in_world_space() {
        let camera = camera();
        let mesh = full_triangle(0.0, color("#FFFFFF", 1.0));
        let mut draw = SurfaceDraw::new(&mesh);
        draw.shading = [0.0; 3];
        draw.depth_test = false;
        draw.clip_planes[0] = [1.0, 0.0, 0.0, 0.0];
        let draws = [ThreeDDraw::Surface(draw)];
        let frame = ThreeDJob::new(&camera, &draws, Tiling::default())
            .expect("job")
            .render(1)
            .expect("frame");
        assert!(pixel(&frame, 24, 12)[0] > 0.9);
        assert!(pixel(&frame, 8, 12)[0] < 0.01);
    }

    #[test]
    fn closed_fill_clipping_preserves_viewport_and_user_plane_corners() {
        let camera = camera();
        // Every original edge lies outside the viewport. Independent
        // per-curve clipping would therefore discard the whole path even
        // though its interior covers every pixel.
        let enclosing = path_points(
            &[
                [-20.0, -20.0, 0.0],
                [20.0, -20.0, 0.0],
                [20.0, 20.0, 0.0],
                [-20.0, 20.0, 0.0],
            ],
            true,
        );
        let plan = vector_plan(
            &enclosing,
            [1.0; 4],
            [0.0; 4],
            0.0,
            camera.revision(),
            |uniforms| {
                uniforms.clip_planes[0] = [1.0, 0.0, 0.0, 0.0];
                uniforms.clip_planes[1] = [0.0, 1.0, 0.0, 0.0];
            },
        );
        let frame = ThreeDJob::new(
            &camera,
            &[ThreeDDraw::Vector(VectorDraw::new(&plan, 0))],
            Tiling::default(),
        )
        .expect("clipped fill job")
        .render(1)
        .expect("frame");

        assert!(
            pixel(&frame, 24, 6)[0] > 0.99,
            "the positive x/y clip quadrant must remain filled"
        );
        assert!(
            pixel(&frame, 8, 6)[0] < 0.01,
            "the x user plane must remove the left quadrant"
        );
        assert!(
            pixel(&frame, 24, 18)[0] < 0.01,
            "the y user plane must remove the lower quadrant"
        );
    }

    #[test]
    fn user_plane_clips_the_stroke_surface_not_only_its_centerline() {
        let camera = camera();
        let line = path_points(&[[-3.0, 0.0, 0.0], [3.0, 0.0, 0.0]], false);
        let plan = vector_plan(
            &line,
            [0.0; 4],
            [1.0; 4],
            400.0,
            camera.revision(),
            |uniforms| uniforms.clip_planes[0] = [1.0, 0.0, 0.0, 0.0],
        );
        let frame = ThreeDJob::new(
            &camera,
            &[ThreeDDraw::Vector(VectorDraw::new(&plan, 0))],
            Tiling::default(),
        )
        .expect("clipped stroke job")
        .render(1)
        .expect("frame");
        assert!(
            pixel(&frame, 15, 12)[0] < 0.01,
            "the thick round cap must not bleed across x=0"
        );
        assert!(
            pixel(&frame, 17, 12)[0] > 0.9,
            "the retained side of the clipped stroke remains visible"
        );
    }

    #[test]
    fn clipping_does_not_reparameterize_a_planar_gradient() {
        let camera = camera();
        let tilted = path_points(
            &[
                [-5.0, -3.0, -0.5],
                [5.0, -3.0, 0.5],
                [5.0, 3.0, 0.5],
                [-5.0, 3.0, -0.5],
            ],
            true,
        );
        let plain = gradient_vector_plan(
            &tilted,
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
            camera.revision(),
            |_| {},
        );
        let clipped = gradient_vector_plan(
            &tilted,
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
            camera.revision(),
            |uniforms| uniforms.clip_planes[0] = [1.0, 0.0, 0.0, 0.0],
        );
        let render = |plan: &RenderPlan| {
            ThreeDJob::new(
                &camera,
                &[ThreeDDraw::Vector(VectorDraw::new(plan, 0))],
                Tiling::default(),
            )
            .expect("gradient job")
            .render(1)
            .expect("gradient frame")
        };
        let plain = pixel(&render(&plain), 22, 12);
        let clipped = pixel(&render(&clipped), 22, 12);
        assert!(
            plain[0] > 0.01 && plain[2] > 0.01,
            "fixture must exercise an interior ramp value: {plain:?}"
        );
        assert_eq!(
            clipped, plain,
            "a visibility plane cannot rewrite the object's color field"
        );
    }

    #[test]
    fn nonplanar_gradient_fill_is_a_named_refusal() {
        let camera = camera();
        let twisted = path_points(
            &[
                [-2.0, -2.0, 0.0],
                [2.0, -2.0, 0.0],
                [2.0, 2.0, 1.0],
                [-2.0, 2.0, 0.0],
            ],
            true,
        );
        let plan = gradient_vector_plan(
            &twisted,
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
            camera.revision(),
            |_| {},
        );
        assert_eq!(
            ThreeDJob::new(
                &camera,
                &[ThreeDDraw::Vector(VectorDraw::new(&plan, 0))],
                Tiling::default(),
            )
            .expect_err("a nonplanar MVC field has no silent 2D substitute"),
            ThreeDError::NonPlanarVectorGradient
        );
    }

    #[test]
    fn nonplanar_shaded_fill_is_a_named_refusal() {
        let camera = camera();
        let twisted = path_points(
            &[
                [-2.0, -2.0, 0.0],
                [2.0, -2.0, 0.0],
                [2.0, 2.0, 1.0],
                [-2.0, 2.0, 0.0],
            ],
            true,
        );
        let plan = vector_plan(
            &twisted,
            [1.0; 4],
            [0.0; 4],
            0.0,
            camera.revision(),
            |uniforms| uniforms.shading = [0.3, 0.2, 0.1],
        );
        assert_eq!(
            ThreeDJob::new(
                &camera,
                &[ThreeDDraw::Vector(VectorDraw::new(&plan, 0))],
                Tiling::default(),
            )
            .expect_err("a nonplanar fill has no silent lighting surface"),
            ThreeDError::NonPlanarVectorShading
        );
    }

    #[test]
    fn homogeneous_clip_volume_rejects_geometry_behind_the_camera() {
        let camera = camera();
        let mesh = full_triangle(20.0, color("#FFFFFF", 1.0));
        let mut draw = SurfaceDraw::new(&mesh);
        draw.shading = [0.0; 3];
        draw.depth_test = false;
        let draws = [ThreeDDraw::Surface(draw)];
        let job = ThreeDJob::new(&camera, &draws, Tiling::default()).expect("job");
        assert_eq!(
            job.draw_count(),
            1,
            "clipped-empty draws retain painter order"
        );
        let frame = job.render(1).expect("frame");
        assert_eq!(pixel(&frame, 16, 12), [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn true_dot_profile_keeps_radial_glow_and_aa() {
        assert_eq!(true_dot_alpha(0.0, 0.1, 2.0), 1.0);
        let midpoint = true_dot_alpha(0.5, 0.1, 2.0);
        assert!((midpoint - 0.25).abs() < 1e-12);
        assert_eq!(true_dot_alpha(1.0, 0.1, 2.0), 0.0);
        assert_eq!(true_dot_alpha(1.1, 0.1, 0.0), 0.0);
    }

    #[test]
    fn perspective_attributes_use_inverse_w_not_screen_linear_weights() {
        let attributes = |u: f64| Attributes {
            world: [u, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0],
            color: [u, 0.0, 0.0, 1.0],
            uv: [u, 0.0],
            opacity: 1.0,
        };
        let triangle = RasterTriangle {
            vertices: [
                RasterVertex {
                    attributes: attributes(0.0),
                    screen: [0.0, 0.0],
                    inverse_w: 1.0,
                    ndc_z: 0.0,
                },
                RasterVertex {
                    attributes: attributes(1.0),
                    screen: [1.0, 0.0],
                    inverse_w: 0.5,
                    ndc_z: 0.0,
                },
                RasterVertex {
                    attributes: attributes(0.0),
                    screen: [0.0, 1.0],
                    inverse_w: 1.0,
                    ndc_z: 0.0,
                },
            ],
            bounds: [0.0, 0.0, 1.0, 1.0],
        };
        let (got, _) =
            perspective_attributes(&triangle, [0.25, 0.5, 0.25]).expect("finite weights");
        assert!((got.uv[0] - 1.0 / 3.0).abs() < 1e-12);
        assert_ne!(got.uv[0], 0.5, "screen-linear interpolation is wrong");
    }

    #[test]
    fn image_quad_preserves_top_left_orientation_through_rasterization() {
        let camera = camera();
        let texture = Texture::from_rgba8(
            2,
            2,
            &[
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
            ],
            crate::texture::TextureEncoding::Linear,
        )
        .expect("texture");
        let scale = camera.frame().scale();
        let half_width = fmn_core::constants::FRAME_WIDTH * scale / 2.0;
        let half_height = fmn_core::constants::FRAME_HEIGHT * scale / 2.0;
        let normal = [0.0, 0.0, 1.0];
        let mesh = SurfaceMesh::from_uv_grid(
            vec![
                SurfaceVertex::textured([-half_width, half_height, 0.0], normal, [0.0, 0.0], 1.0),
                SurfaceVertex::textured([-half_width, -half_height, 0.0], normal, [0.0, 1.0], 1.0),
                SurfaceVertex::textured([half_width, half_height, 0.0], normal, [1.0, 0.0], 1.0),
                SurfaceVertex::textured([half_width, -half_height, 0.0], normal, [1.0, 1.0], 1.0),
            ],
            (2, 2),
        )
        .expect("image quad");
        let draw = SurfaceDraw::image(&mesh, &texture);
        let draws = [ThreeDDraw::Surface(draw)];
        let frame = ThreeDJob::new(&camera, &draws, Tiling::default())
            .expect("job")
            .render(1)
            .expect("frame");

        let red = pixel(&frame, 8, 6);
        let green = pixel(&frame, 24, 6);
        let blue = pixel(&frame, 8, 18);
        let white = pixel(&frame, 24, 18);
        assert!(red[0] > 0.8 && red[1] < 0.1 && red[2] < 0.1);
        assert!(green[0] < 0.1 && green[1] > 0.8 && green[2] < 0.1);
        assert!(blue[0] < 0.1 && blue[1] < 0.1 && blue[2] > 0.8);
        assert!(white[0] > 0.8 && white[1] > 0.8 && white[2] > 0.8);
    }

    #[test]
    fn textured_surface_dark_shift_and_alpha_policy_match_the_reference() {
        let light = Texture::from_rgba8(
            1,
            1,
            &[255, 255, 255, 64],
            crate::texture::TextureEncoding::Linear,
        )
        .expect("light");
        let dark = Texture::from_rgba8(
            1,
            1,
            &[0, 0, 0, 255],
            crate::texture::TextureEncoding::Linear,
        )
        .expect("dark");
        let attributes = Attributes {
            world: [0.0; 3],
            normal: normalize(crate::camera::DEFAULT_LIGHT_POSITION).expect("light direction"),
            color: [1.0; 4],
            uv: [0.5, 0.5],
            opacity: 0.5,
        };
        let surface = Shader::Texture {
            material: TextureMaterial::surface(&light, Some(&dark)),
            shading: [0.0; 3],
            light_position: crate::camera::DEFAULT_LIGHT_POSITION,
            camera_position: [0.0, 0.0, 10.0],
        };
        let lit = shade(surface, attributes).expect("opaque-enough surface sample");
        assert!(lit.r > 0.99 && lit.g > 0.99 && lit.b > 0.99);
        assert_eq!(lit.a, 0.5, "TexturedSurface overrides sampled alpha");

        let dark_attributes = Attributes {
            normal: mul(attributes.normal, -1.0),
            ..attributes
        };
        let unlit = shade(surface, dark_attributes).expect("dark sample");
        assert!(unlit.r < 0.01 && unlit.g < 0.01 && unlit.b < 0.01);

        let interpolated_normal = Attributes {
            normal: mul(attributes.normal, 0.1),
            ..attributes
        };
        let transition = shade(surface, interpolated_normal).expect("transition sample");
        assert!(
            (transition.r - smoothstep(-DARK_SHIFT, DARK_SHIFT, 0.1)).abs() < 2e-6,
            "the fragment shader uses the interpolated unit-normal varying \
             without renormalizing it"
        );

        let image = Shader::Texture {
            material: TextureMaterial::image(&light),
            shading: [0.0; 3],
            light_position: crate::camera::DEFAULT_LIGHT_POSITION,
            camera_position: [0.0, 0.0, 10.0],
        };
        let image_sample = shade(image, attributes).expect("image sample");
        assert!((image_sample.a - (64.0 / 255.0) * 0.5).abs() < 2e-6);
    }

    #[test]
    fn shared_triangle_edge_is_half_open() {
        let attributes = Attributes {
            world: [0.0; 3],
            normal: [0.0, 0.0, 1.0],
            color: [1.0; 4],
            uv: [0.0; 2],
            opacity: 0.5,
        };
        let vertex = |screen| RasterVertex {
            attributes,
            screen,
            inverse_w: 1.0,
            ndc_z: 0.0,
        };
        let first = RasterTriangle {
            vertices: [vertex([0.0, 0.0]), vertex([2.0, 0.0]), vertex([0.0, 2.0])],
            bounds: [0.0, 0.0, 2.0, 2.0],
        };
        let second = RasterTriangle {
            vertices: [vertex([2.0, 0.0]), vertex([2.0, 2.0]), vertex([0.0, 2.0])],
            bounds: [0.0, 0.0, 2.0, 2.0],
        };
        let sample = [1.0, 1.0];
        let accepted = [&first, &second]
            .into_iter()
            .filter(|triangle| barycentric(triangle, sample).is_some())
            .count();
        assert_eq!(accepted, 1, "a shared edge must composite exactly once");
    }

    #[test]
    fn moved_light_changes_the_lighting_frame_and_threads_do_not() {
        let mut camera = camera();
        let mesh = full_triangle(0.0, color("#666666", 1.0));
        let draw = SurfaceDraw::new(&mesh);
        let draws = [ThreeDDraw::Surface(draw)];
        let first = ThreeDJob::new(&camera, &draws, Tiling::default())
            .expect("job")
            .render(1)
            .expect("frame");

        camera
            .set_light_source_position([0.0, 0.0, -10.0])
            .expect("light");
        let moved_job = ThreeDJob::new(&camera, &draws, Tiling::default()).expect("job");
        let moved_one = moved_job.render(1).expect("frame");
        let moved_four = moved_job.render(4).expect("frame");
        let moved_sixteen = moved_job.render(16).expect("frame");
        assert_ne!(sha256(first.as_bytes()), sha256(moved_one.as_bytes()));
        assert_eq!(moved_one.as_bytes(), moved_four.as_bytes());
        assert_eq!(moved_one.as_bytes(), moved_sixteen.as_bytes());
    }

    #[test]
    fn mixed_vector_camera_derivation_is_revisioned_bit_locked_and_thread_independent() {
        let mut camera = camera();
        let square = path_points(
            &[
                [-2.5, -2.0, 0.5],
                [2.5, -2.0, 0.5],
                [2.5, 2.0, 0.5],
                [-2.5, 2.0, 0.5],
            ],
            true,
        );
        let plan = vector_plan(
            &square,
            [0.0, 0.5, 1.0, 0.7],
            [1.0, 1.0, 1.0, 1.0],
            8.0,
            camera.revision(),
            |uniforms| {
                uniforms.flat_stroke = true;
                uniforms.scale_stroke_with_zoom = true;
            },
        );
        let original_segments = plan.segments().to_vec();
        let surface = full_triangle(-1.5, color("#301050", 1.0));
        let mut surface_draw = SurfaceDraw::new(&surface);
        surface_draw.shading = [0.0; 3];
        surface_draw.depth_test = false;
        let draws = [
            ThreeDDraw::Surface(surface_draw),
            ThreeDDraw::Vector(VectorDraw::new(&plan, 0)),
            ThreeDDraw::TrueDot(TrueDotDraw::glow(
                [0.0, 0.0, 1.0],
                0.8,
                color("#FF8030", 0.8),
            )),
        ];
        let before = ThreeDJob::new(&camera, &draws, Tiling::default()).expect("job");
        assert_eq!(before.camera_revision(), camera.revision());
        let one = before.render(1).expect("frame");
        let four = before.render(4).expect("frame");
        let sixteen = before.render(16).expect("frame");
        assert_eq!(one.as_bytes(), four.as_bytes());
        assert_eq!(one.as_bytes(), sixteen.as_bytes());
        // Adjudicated 2026-07-28 (fm-diu): perspective strokes now minimize
        // `distance - half_width` per curve before choosing the winning color,
        // width and depth. The previous digest is restored if that law is
        // reverted, and
        // `perspective_stroke_minimizes_signed_excess_before_choosing_a_curve`
        // independently proves why nearest-curve-first is the wrong image.
        assert_eq!(
            sha256(one.as_bytes()).to_hex(),
            "e13a704147fec87adc9c70bab4a3ef82725175a60790eda2d7d340b75ec0982f",
            "mixed vector/3D self-golden"
        );

        let old_revision = camera.revision();
        camera
            .frame_mut()
            .set_center([0.75, 0.0, 0.0])
            .expect("camera move");
        let moved = ThreeDJob::new(&camera, &draws, Tiling::default()).expect("moved job");
        assert!(moved.camera_revision() > old_revision);
        assert_eq!(
            plan.segments(),
            original_segments,
            "camera invalidation must not mutate object-space geometry"
        );
        assert_ne!(
            sha256(one.as_bytes()),
            sha256(moved.render(1).expect("moved frame").as_bytes())
        );
    }
}
