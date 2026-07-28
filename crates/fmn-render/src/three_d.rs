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

use crate::bin::Tiling;
use crate::camera::{Camera, EdgeSampleLimit};
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
    triangles: Vec<RasterTriangle>,
    shader: Shader<'a>,
    depth_test: bool,
    bounds: Option<[f64; 4]>,
}

/// Camera-bound, clipped, tiled 3D frame.
#[derive(Debug)]
pub struct ThreeDJob<'a> {
    camera: &'a Camera,
    draws: Vec<CompiledDraw<'a>>,
    tiling: Tiling,
    sample_grid: u32,
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
                for triangle in &draw.triangles {
                    scratch.mark_triangle_boundary(triangle, rectangle, width, height);
                }
            }
        }

        for draw in &self.draws {
            if !bounds_intersect(draw.bounds, rectangle) {
                continue;
            }
            for triangle in &draw.triangles {
                scratch.raster_triangle(triangle, draw, rectangle, width, height);
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
        triangles,
        shader,
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

    fn raster_triangle(
        &mut self,
        triangle: &RasterTriangle,
        draw: &CompiledDraw<'_>,
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
                        if draw.depth_test && depth >= self.depth[slot] {
                            continue;
                        }
                        let Some(source) = shade(draw.shader, attributes) else {
                            continue;
                        };
                        self.color[slot] = source_over(source, self.color[slot]);
                        if draw.depth_test {
                            self.depth[slot] = depth;
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
}
