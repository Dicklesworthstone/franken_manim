//! `ThreeDModel` and the owned OBJ-subset reader (§12.4, fm-2u6; Appendix A
//! `types/surface.py:ThreeDModel`).
//!
//! The Reference loads a Wavefront OBJ through trimesh (with a pywavefront
//! pass for MTL textures). Both are displaced here by a small owned
//! parser — the governed closure admits no new dependency for a text
//! format — reading exactly the geometry subset a model needs:
//!
//! # The supported subset (and its documented limits)
//!
//! | Statement | Support |
//! |---|---|
//! | `v x y z [w]` | yes; the homogeneous `w` is accepted and ignored |
//! | `vn i j k` | yes; stored raw, normalized at mobject conversion |
//! | `vt u [v [w]]` | yes; `v` defaults to 0, `w` ignored |
//! | `f c₁ c₂ …` | yes; corner forms `v`, `v/vt`, `v//vn`, `v/vt/vn`; 1-based and negative (relative) indices; planar simple convex/concave polygons |
//! | `s off`, `s 0`, `s on`, `s positive_integer` | hard faces, or area-weighted corner normals within each smoothing group |
//! | `o`, `g`, `usemtl`, `mtllib`, `l`, `p`, `vp` | parsed over (no effect) |
//! | anything else | skipped (trimesh-compatible tolerance) |
//! | `#` comments, blank lines, CRLF | handled |
//!
//! This geometry-only reader ignores material directives; [`crate::ObjDocument`]
//! resolves material runs over the same parsed triangles. Free-form curves and
//! surfaces (`cstype` etc.) remain skipped. There is no axis remap.
//!
//! Polygons preserve every distinct corner's UV/normal indices, including
//! collinear boundary corners. An identical repeated closing corner is
//! accepted once; coincident corners with different attributes are refused.
//! Valid convex fans retain their original triangle order. Other simple
//! polygons use Chisel's bounded, boundary-preserving ear clipper after a
//! stable dominant-plane projection. Faces must be planar within `1e-6` of
//! their largest 3D bounding-box extent; admission never moves their original
//! positions. Self-intersections, touching/backtracking boundaries, excessive
//! work, and ambiguous degeneracies are named refusals. Existing explicit
//! three-corner faces, including zero-area ones, retain their direct behavior.
//!
//! Smoothing generates only missing corner normals; authored `vn` values keep
//! precedence. Every source polygon contributes its area once per shared vertex,
//! independently of triangulation, UV seams, object names or material runs.
//! Different smoothing groups retain hard edges. Generated normals are appended
//! after all input is parsed, so positive and negative authored normal indices
//! still refer only to declarations visible at their face. Degenerate or exactly
//! cancelling smooth neighborhoods retain the existing face-normal fallback.
//! Face normals and height normalization are independent of the source units:
//! finite tiny or very large models keep their geometry and lighting after
//! normalization, without overflowing an intermediate area, center or scale.
//!
//! # Untrusted input (§16.5/R14)
//!
//! The parser is line-based and total: every malformed byte sequence
//! yields a typed [`ObjError`] naming the line and the token — never a
//! panic — and resource budgets ([`ObjLimits`]) are declared before any
//! allocation, refusing over-budget input with a named error. It is a
//! target of the W10 fuzz campaign (`obj_model` in fmn-conformance's
//! `tests/fuzz_campaign.rs`).

use std::ops::Range;

use fmn_core::color::Srgb;
use fmn_core::constants::GREY;
use fmn_core::types::Vec3;
use fmn_geom::{
    EarClipError, EarClipOptions, MAX_EARCLIP_VERTICES, triangulate_preserving_vertices,
};
use fmn_mobject::record::{RecordBuffer, RecordSchema};
use fmn_mobject::uniforms::Uniforms;
use fmn_mobject::{Mobject, RenderPrimitive};

#[path = "obj_smoothing.rs"]
mod smoothing;

/// The Reference's default `height` for `ThreeDModel` (scene units).
pub const DEFAULT_MODEL_HEIGHT: f64 = 3.0;

/// The kept Surface shading default `(reflectiveness, gloss, shadow)` —
/// G0-2 decision row (e) (`surface.py:47`), which `ThreeDModel`'s
/// `TexturedSurface` children inherit.
pub const MODEL_SHADING: Vec3 = [0.3, 0.2, 0.4];

// ------------------------------------------------------------------ errors

/// Why byte input could not become an [`ObjMesh`]. Every variant names
/// the 1-based line; float/index variants name the offending token.
#[derive(Debug, Clone, PartialEq)]
pub enum ObjError {
    /// The input is not UTF-8; `offset` is the first invalid byte.
    NotUtf8 {
        /// Byte offset of the first invalid UTF-8 sequence.
        offset: usize,
    },
    /// A float token did not parse as a finite f64 (`nan`/`inf` parse but
    /// are rejected — geometry must be finite).
    BadFloat {
        /// 1-based line number.
        line: usize,
        /// The statement being parsed (`v`, `vn`, `vt`).
        statement: &'static str,
        /// The offending token.
        token: String,
    },
    /// An index token did not parse as an integer, or a corner had too
    /// many `/` fields.
    BadIndex {
        /// 1-based line number.
        line: usize,
        /// The offending token.
        token: String,
    },
    /// A smoothing directive does not name off, on, or one unsigned group ID.
    BadSmoothingGroup {
        /// 1-based line number.
        line: usize,
        /// The malformed directive argument(s).
        token: String,
    },
    /// An index resolved outside its declared list. OBJ indices are
    /// 1-based; `0` is always out of range.
    IndexOutOfRange {
        /// 1-based line number.
        line: usize,
        /// The index as written (after sign, before resolution).
        index: i64,
        /// The declared length of the referenced list.
        len: usize,
    },
    /// A face named fewer than three corners.
    FaceTooSmall {
        /// 1-based line number.
        line: usize,
        /// How many corners the face named.
        corners: usize,
    },
    /// A polygon named more corners than the bounded geometry kernel accepts.
    FaceTooLarge {
        /// 1-based line number.
        line: usize,
        /// The maximum number of source face corners.
        limit: usize,
    },
    /// A polygon has no stable plane or exceeds the planarity tolerance.
    InvalidPolygon {
        /// 1-based line number.
        line: usize,
        /// The geometric admission failure.
        reason: &'static str,
    },
    /// The projected boundary cannot be triangulated without losing corners.
    Triangulation {
        /// 1-based line number.
        line: usize,
        /// The geometry kernel's typed refusal.
        cause: EarClipError,
    },
    /// The vertex budget refused.
    TooManyVertices {
        /// 1-based line number.
        line: usize,
        /// The configured budget.
        limit: usize,
    },
    /// The texture-coordinate budget refused.
    TooManyTexCoords {
        /// 1-based line number.
        line: usize,
        /// The configured budget.
        limit: usize,
    },
    /// The normal budget refused.
    TooManyNormals {
        /// 1-based line number.
        line: usize,
        /// The configured budget.
        limit: usize,
    },
    /// The emitted triangle budget refused.
    TooManyTriangles {
        /// 1-based line number.
        line: usize,
        /// The configured budget.
        limit: usize,
    },
    /// Bounded smoothing storage could not be reserved.
    AllocationFailed {
        /// 1-based line number.
        line: usize,
        /// The refused storage class.
        resource: &'static str,
        /// Number of records requested.
        requested: usize,
    },
}

impl std::fmt::Display for ObjError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotUtf8 { offset } => {
                write!(
                    f,
                    "input is not UTF-8 (first invalid byte at offset {offset})"
                )
            }
            Self::BadFloat {
                line,
                statement,
                token,
            } => write!(
                f,
                "line {line}: `{statement}` coordinate {token:?} is not a finite float"
            ),
            Self::BadIndex { line, token } => {
                write!(f, "line {line}: face index {token:?} is not an integer")
            }
            Self::BadSmoothingGroup { line, token } => write!(
                f,
                "line {line}: smoothing group {token:?} must be off, on, or one unsigned integer"
            ),
            Self::IndexOutOfRange { line, index, len } => write!(
                f,
                "line {line}: index {index} resolves out of range \
                 ({len} declared; OBJ indices are 1-based, negatives relative)"
            ),
            Self::FaceTooSmall { line, corners } => write!(
                f,
                "line {line}: face names {corners} corner(s); at least 3 are required"
            ),
            Self::FaceTooLarge { line, limit } => {
                write!(f, "line {line}: face corner budget of {limit} exceeded")
            }
            Self::InvalidPolygon { line, reason } => {
                write!(f, "line {line}: invalid polygon: {reason}")
            }
            Self::Triangulation { line, cause } => {
                write!(f, "line {line}: polygon triangulation refused: {cause}")
            }
            Self::TooManyVertices { line, limit } => {
                write!(f, "line {line}: vertex budget of {limit} exceeded")
            }
            Self::TooManyTexCoords { line, limit } => {
                write!(
                    f,
                    "line {line}: texture-coordinate budget of {limit} exceeded"
                )
            }
            Self::TooManyNormals { line, limit } => {
                write!(f, "line {line}: normal budget of {limit} exceeded")
            }
            Self::TooManyTriangles { line, limit } => {
                write!(f, "line {line}: triangle budget of {limit} exceeded")
            }
            Self::AllocationFailed {
                line,
                resource,
                requested,
            } => write!(
                f,
                "line {line}: could not reserve {requested} OBJ {resource} records"
            ),
        }
    }
}

impl std::error::Error for ObjError {}

// ------------------------------------------------------------------ limits

/// Resource budgets for the OBJ reader, declared before any allocation
/// (§16.5/R14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjLimits {
    /// Maximum `v` statements.
    pub max_vertices: usize,
    /// Maximum `vt` statements.
    pub max_tex_coords: usize,
    /// Maximum total normals, including generated smoothing-group normals.
    pub max_normals: usize,
    /// Maximum emitted triangles (after any identical closing corner is removed).
    pub max_triangles: usize,
}

impl Default for ObjLimits {
    fn default() -> Self {
        Self {
            max_vertices: 1 << 20,
            max_tex_coords: 1 << 20,
            max_normals: 1 << 20,
            max_triangles: 1 << 21,
        }
    }
}

// -------------------------------------------------------------------- mesh

/// One corner of a triangle: the vertex plus its optional texture
/// coordinate and normal, resolved to 0-based indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjCorner {
    /// 0-based index into [`ObjMesh::vertices`].
    pub vertex: usize,
    /// 0-based index into [`ObjMesh::tex_coords`], if the corner named one.
    pub tex_coord: Option<usize>,
    /// 0-based index into [`ObjMesh::normals`], authored or generated by smoothing.
    pub normal: Option<usize>,
}

/// A parsed OBJ mesh: the declared attribute lists and triangulated faces.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjMesh {
    /// `v` statements, in declaration order.
    pub vertices: Vec<Vec3>,
    /// `vt` statements (`v` defaulted to 0), in declaration order.
    pub tex_coords: Vec<[f64; 2]>,
    /// `vn` statements, raw as declared (normalized at conversion), followed
    /// by generated unit normals for missing corners in smoothing groups.
    pub normals: Vec<Vec3>,
    /// Triangulated faces, retaining source face order and original winding.
    pub triangles: Vec<[ObjCorner; 3]>,
}

/// A source face's one-based line and its emitted triangle interval.
type ObjFaceRange = (usize, Range<usize>);

impl ObjMesh {
    /// Parse OBJ bytes under the default budgets.
    pub fn parse(bytes: &[u8]) -> Result<Self, ObjError> {
        Self::parse_with_limits(bytes, &ObjLimits::default())
    }

    /// Parse OBJ bytes under explicit budgets (the untrusted-input path).
    pub fn parse_with_limits(bytes: &[u8], limits: &ObjLimits) -> Result<Self, ObjError> {
        Self::parse_with_face_sink(bytes, limits, |_, _| {})
    }

    /// Material bindings consume the geometry parser's actual face intervals,
    /// never a second guess at how many triangles a source polygon emitted.
    pub(crate) fn parse_with_face_ranges(
        bytes: &[u8],
        limits: &ObjLimits,
    ) -> Result<(Self, Vec<ObjFaceRange>), ObjError> {
        let mut faces = Vec::new();
        let mesh = Self::parse_with_face_sink(bytes, limits, |line, triangles| {
            faces.push((line, triangles));
        })?;
        Ok((mesh, faces))
    }

    fn parse_with_face_sink(
        bytes: &[u8],
        limits: &ObjLimits,
        mut on_face: impl FnMut(usize, Range<usize>),
    ) -> Result<Self, ObjError> {
        let text = std::str::from_utf8(bytes).map_err(|e| ObjError::NotUtf8 {
            offset: e.valid_up_to(),
        })?;
        let mut mesh = Self {
            vertices: Vec::new(),
            tex_coords: Vec::new(),
            normals: Vec::new(),
            triangles: Vec::new(),
        };
        let mut smoothing = smoothing::Smoothing::default();
        for (line_no, raw_line) in text.lines().enumerate() {
            let line = line_no + 1;
            // `#` starts a comment; surrounding whitespace is insignificant.
            let content = raw_line.split('#').next().unwrap_or("").trim();
            if content.is_empty() {
                continue;
            }
            let mut tokens = content.split_whitespace();
            let statement = tokens.next().unwrap_or("");
            // Bound corner storage before allocation; ignored directives and
            // optional coordinates never require collecting an unbounded line.
            let args: Vec<&str> = match statement {
                "f" => tokens.take(MAX_EARCLIP_VERTICES + 1).collect(),
                "v" | "vn" => tokens.take(3).collect(),
                "vt" => tokens.take(2).collect(),
                "s" => tokens.take(2).collect(),
                _ => Vec::new(),
            };
            match statement {
                "v" => {
                    if mesh.vertices.len() >= limits.max_vertices {
                        return Err(ObjError::TooManyVertices {
                            line,
                            limit: limits.max_vertices,
                        });
                    }
                    let xyz = parse_floats(&args, line, "v")?;
                    mesh.vertices.push(xyz);
                }
                "vn" => {
                    if mesh.normals.len().saturating_add(smoothing.normal_count())
                        >= limits.max_normals
                    {
                        return Err(ObjError::TooManyNormals {
                            line,
                            limit: limits.max_normals,
                        });
                    }
                    let ijk = parse_floats(&args, line, "vn")?;
                    mesh.normals.push(ijk);
                }
                "vt" => {
                    if mesh.tex_coords.len() >= limits.max_tex_coords {
                        return Err(ObjError::TooManyTexCoords {
                            line,
                            limit: limits.max_tex_coords,
                        });
                    }
                    if args.is_empty() {
                        return Err(ObjError::BadFloat {
                            line,
                            statement: "vt",
                            token: String::new(),
                        });
                    }
                    let u = parse_float(args[0], line, "vt")?;
                    let v = match args.get(1) {
                        Some(token) => parse_float(token, line, "vt")?,
                        None => 0.0,
                    };
                    mesh.tex_coords.push([u, v]);
                }
                "f" => {
                    if args.len() < 3 {
                        return Err(ObjError::FaceTooSmall {
                            line,
                            corners: args.len(),
                        });
                    }
                    if args.len() > MAX_EARCLIP_VERTICES {
                        return Err(ObjError::FaceTooLarge {
                            line,
                            limit: MAX_EARCLIP_VERTICES,
                        });
                    }
                    let mut corners = Vec::with_capacity(args.len());
                    for token in &args {
                        corners.push(parse_corner(token, line, &mesh)?);
                    }
                    // Only a fully identical closing corner is redundant.
                    // Different UV/normal indices must never disappear.
                    if corners.len() > 3 && corners.first() == corners.last() {
                        corners.pop();
                    }
                    let start = mesh.triangles.len();
                    let end = start
                        .checked_add(corners.len() - 2)
                        .filter(|&end| end <= limits.max_triangles)
                        .ok_or(ObjError::TooManyTriangles {
                            line,
                            limit: limits.max_triangles,
                        })?;
                    if args.len() == 3 {
                        mesh.triangles.push([corners[0], corners[1], corners[2]]);
                    } else {
                        for triangle in triangulate_face(&mesh, &corners, line)? {
                            mesh.triangles.push(triangle.map(|index| corners[index]));
                        }
                    }
                    debug_assert_eq!(mesh.triangles.len(), end);
                    smoothing.record_face(&mesh, &corners, start..end, line, limits)?;
                    on_face(line, start..end);
                }
                "s" => smoothing.set_group(&args, line)?,
                // Known no-ops for the geometry subset (grouping,
                // materials, lines, points, parameter-space vertices).
                "o" | "g" | "usemtl" | "mtllib" | "l" | "p" | "vp" => {}
                // Unknown statements are skipped, trimesh-compatible.
                _ => {}
            }
        }
        smoothing.finish(&mut mesh)?;
        Ok(mesh)
    }

    /// No triangles.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }

    /// Declared vertex count.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.vertices.len()
    }

    /// Emitted triangle count.
    #[must_use]
    pub fn triangle_count(&self) -> usize {
        self.triangles.len()
    }

    /// The geometric normal of a triangle: `cross(v1-v0, v2-v0)`
    /// normalized, or `[0, 0, 0]` for a degenerate (zero-area) triangle —
    /// a zero normal is a defined value the lighting treats as unlit,
    /// where a NaN would poison the fragment.
    #[must_use]
    pub fn face_normal(&self, triangle: usize) -> Vec3 {
        let Some(corners) = self.triangles.get(triangle) else {
            return [0.0; 3];
        };
        let v0 = self.vertices[corners[0].vertex];
        let v1 = self.vertices[corners[1].vertex];
        let v2 = self.vertices[corners[2].vertex];
        let a = sub(v1, v0);
        let b = sub(v2, v0);
        let normal = cross(a, b);
        let squared = normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2];
        if squared.is_normal() {
            // Preserve ordinary meshes' arithmetic and certified output.
            return normalized(normal);
        }
        let represented_products = [
            (a[1], b[2]),
            (a[2], b[1]),
            (a[2], b[0]),
            (a[0], b[2]),
            (a[0], b[1]),
            (a[1], b[0]),
        ]
        .into_iter()
        .all(|(x, y)| x == 0.0 || y == 0.0 || (x * y).is_normal());
        if represented_products
            && normal != [0.0; 3]
            && normal
                .iter()
                .all(|value| *value == 0.0 || value.is_normal())
        {
            // The cross product itself is still usable when only its squared
            // length over/underflows. Rescaling anisotropic edges again could
            // erase a component that the original products represented.
            return normalized(normal);
        }
        if normal == [0.0; 3] && represented_products {
            // An ordinary exactly collinear face stays degenerate. Rescaling
            // its independently rounded edges must not invent a tiny normal.
            // Check every product: a skinny face can lose its area even when
            // the product of the two largest edge components is normal.
            return [0.0; 3];
        }
        // The smoothing accumulator already keeps areas in scale² units and
        // handles overflowing edge subtraction. Reuse that same geometry for
        // hard faces whose unscaled cross product cannot carry the direction.
        smoothing::face_normal(self, corners)
    }

    /// The normal of one corner: its authored or generated normal normalized, falling
    /// back to the geometric face normal when the corner named none (or a
    /// degenerate zero normal).
    #[must_use]
    pub fn corner_normal(&self, triangle: usize, corner: usize) -> Vec3 {
        let face = self.face_normal(triangle);
        let Some(corners) = self.triangles.get(triangle) else {
            return face;
        };
        let Some(corner) = corners.get(corner) else {
            return face;
        };
        match corner.normal {
            Some(index) => match self.normals.get(index) {
                Some(&vn) => {
                    let n = normalized(vn);
                    if n == [0.0; 3] { face } else { n }
                }
                None => face,
            },
            None => face,
        }
    }
}

/// Project a planar source polygon into bounded predicate coordinates. The
/// cyclic axis pairs preserve the sign of the corresponding 3D normal.
fn triangulate_face(
    mesh: &ObjMesh,
    corners: &[ObjCorner],
    line: usize,
) -> Result<Vec<[usize; 3]>, ObjError> {
    let fail = |reason| ObjError::InvalidPolygon { line, reason };
    let source_origin = mesh.vertices[corners[0].vertex];
    let mut points: Vec<Vec3> = corners
        .iter()
        .map(|corner| sub(mesh.vertices[corner.vertex], source_origin))
        .collect();
    if points.iter().flatten().any(|value| !value.is_finite()) {
        // Opposite finite extremes can overflow subtraction. Scale absolute
        // coordinates first only in that case; ordinary translation preserves
        // tiny in-plane detail beside a huge constant orthogonal coordinate.
        for (point, corner) in points.iter_mut().zip(corners) {
            *point = mesh.vertices[corner.vertex];
        }
    }
    let mut magnitude = 0.0_f64;
    for value in points.iter().flatten() {
        magnitude = magnitude.max(value.abs());
    }
    if magnitude == 0.0 {
        return Err(fail("all corners coincide"));
    }
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for point in &points {
        for axis in 0..3 {
            let value = point[axis] / magnitude;
            min[axis] = min[axis].min(value);
            max[axis] = max[axis].max(value);
        }
    }
    let center = std::array::from_fn::<_, 3, _>(|axis| min[axis] + (max[axis] - min[axis]) * 0.5);
    let span = (max[0] - min[0]).max(max[1] - min[1]).max(max[2] - min[2]);
    if span == 0.0 {
        return Err(fail("all corners coincide"));
    }
    for point in &mut points {
        for axis in 0..3 {
            point[axis] = (point[axis] / magnitude - center[axis]) / span;
        }
    }
    let origin = points[0];
    let mut area_vector = [0.0; 3];
    for i in 0..points.len() {
        let area = cross(
            sub(points[i], origin),
            sub(points[(i + 1) % points.len()], origin),
        );
        for axis in 0..3 {
            area_vector[axis] += area[axis];
        }
    }
    let mut dominant = 0;
    for axis in 1..3 {
        if area_vector[axis].abs() > area_vector[dominant].abs() {
            dominant = axis;
        }
    }
    if area_vector[dominant].abs() <= 1e-24 {
        return Err(fail("no stable nonzero polygon area"));
    }
    let length = (area_vector[0] * area_vector[0]
        + area_vector[1] * area_vector[1]
        + area_vector[2] * area_vector[2])
        .sqrt();
    let normal = area_vector.map(|component| component / length);
    for &point in &points {
        let delta = sub(point, origin);
        let distance = delta[0] * normal[0] + delta[1] * normal[1] + delta[2] * normal[2];
        if distance.abs() > 1e-6 {
            return Err(fail(
                "nonplanar face exceeds the relative 1e-6 tolerance; triangulate it explicitly",
            ));
        }
    }
    let axes = match dominant {
        0 => [1, 2],
        1 => [2, 0],
        _ => [0, 1],
    };
    let projected: Vec<[f64; 2]> = points
        .iter()
        .map(|point| [point[axes[0]], point[axes[1]]])
        .collect();
    let triangles = triangulate_preserving_vertices(&projected)
        .map_err(|cause| ObjError::Triangulation { line, cause })?;
    let orientation = if area_vector[dominant] > 0.0 {
        1.0
    } else {
        -1.0
    };
    let turn = |a: usize, b: usize, c: usize| {
        let a = projected[a];
        let b = projected[b];
        let c = projected[c];
        ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])) * orientation
    };
    let mut min2 = [f64::INFINITY; 2];
    let mut max2 = [f64::NEG_INFINITY; 2];
    for point in &projected {
        for axis in 0..2 {
            min2[axis] = min2[axis].min(point[axis]);
            max2[axis] = max2[axis].max(point[axis]);
        }
    }
    let tolerance = EarClipOptions::default().tolerance;
    let dx = max2[0] - min2[0];
    let dy = max2[1] - min2[1];
    let sliver_eps = tolerance * tolerance * (dx * dx + dy * dy);
    let n = corners.len();
    let convex = (0..n).all(|i| turn(i, (i + 1) % n, (i + 2) % n) >= 0.0);
    if convex && (1..n - 1).all(|i| turn(0, i, i + 1) > sliver_eps) {
        // Keep the established source-order fan, including clockwise input,
        // whenever that fan represents every boundary corner nondegenerately.
        return Ok((1..n - 1).map(|i| [0, i, i + 1]).collect());
    }
    Ok(triangles
        .into_iter()
        .map(|triangle| {
            let mut triangle = triangle.map(|index| index as usize);
            if orientation < 0.0 {
                triangle.swap(1, 2);
            }
            triangle
        })
        .collect())
}

/// Parse the first three tokens of a statement as finite f64s; a short
/// argument list reports the first missing slot as an empty token (a
/// statement that ran out of coordinates is malformed the same way a
/// non-numeric one is).
fn parse_floats(args: &[&str], line: usize, statement: &'static str) -> Result<Vec3, ObjError> {
    let mut out = [0.0; 3];
    for (slot, value) in out.iter_mut().enumerate() {
        let token = args.get(slot).copied().unwrap_or("");
        *value = parse_float(token, line, statement)?;
    }
    Ok(out)
}

/// One finite f64 token. `f64::from_str` accepts `nan`/`inf` spellings;
/// geometry must be finite, so those are refused here.
fn parse_float(token: &str, line: usize, statement: &'static str) -> Result<f64, ObjError> {
    match token.parse::<f64>() {
        Ok(value) if value.is_finite() => Ok(value),
        _ => Err(ObjError::BadFloat {
            line,
            statement,
            token: token.to_owned(),
        }),
    }
}

/// Parse one face corner: `v`, `v/vt`, `v//vn`, or `v/vt/vn`, resolving
/// 1-based and negative (relative) indices against the lists declared so
/// far (OBJ semantics: an index may only reference earlier statements).
fn parse_corner(token: &str, line: usize, mesh: &ObjMesh) -> Result<ObjCorner, ObjError> {
    let fields: Vec<&str> = token.split('/').collect();
    if fields.len() > 3 {
        return Err(ObjError::BadIndex {
            line,
            token: token.to_owned(),
        });
    }
    let parse_index = |raw: &str, line: usize| -> Result<i64, ObjError> {
        raw.parse::<i64>().map_err(|_| ObjError::BadIndex {
            line,
            token: raw.to_owned(),
        })
    };
    let vertex = resolve_index(parse_index(fields[0], line)?, mesh.vertices.len(), line)?;
    let tex_coord = match fields.get(1) {
        Some(&"") | None => None,
        Some(raw) => Some(resolve_index(
            parse_index(raw, line)?,
            mesh.tex_coords.len(),
            line,
        )?),
    };
    let normal = match fields.get(2) {
        Some(&"") | None => None,
        Some(raw) => Some(resolve_index(
            parse_index(raw, line)?,
            mesh.normals.len(),
            line,
        )?),
    };
    Ok(ObjCorner {
        vertex,
        tex_coord,
        normal,
    })
}

/// Resolve an OBJ index to 0-based: positive is 1-based absolute,
/// negative is relative to the current list length, zero is invalid.
fn resolve_index(raw: i64, len: usize, line: usize) -> Result<usize, ObjError> {
    let resolved = if raw > 0 {
        raw - 1
    } else {
        #[allow(clippy::cast_possible_wrap)]
        let len = len as i64;
        len + raw
    };
    #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
    let out_of_range = resolved < 0 || resolved >= len as i64;
    if out_of_range {
        return Err(ObjError::IndexOutOfRange {
            line,
            index: raw,
            len,
        });
    }
    #[allow(clippy::cast_sign_loss)]
    Ok(resolved as usize)
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn largest_component(v: Vec3) -> f64 {
    v.into_iter().map(f64::abs).fold(0.0, f64::max)
}

/// Normalize without interpreting source units as a degeneracy tolerance.
/// Only a zero vector has no direction; small authored normals still override
/// the face normal. Keep the ordinary path's operation order unchanged.
fn normalized(v: Vec3) -> Vec3 {
    let squared = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
    if squared.is_normal() {
        let len = squared.sqrt();
        return [v[0] / len, v[1] / len, v[2] / len];
    }
    let largest = largest_component(v);
    if largest == 0.0 {
        return [0.0; 3];
    }
    let bounded = v.map(|value| value / largest);
    let len = (bounded[0] * bounded[0] + bounded[1] * bounded[1] + bounded[2] * bounded[2]).sqrt();
    bounded.map(|value| value / len)
}

// -------------------------------------------------------------- ThreeDModel

/// The shared geometry placement for plain and material-bearing OBJ models.
/// Exact affine centers preserve the established arithmetic. Compensated
/// centers and bounded ratios avoid overflowing an extent or scale and retain
/// midpoint residuals. Validation of requested heights and final f32 record
/// values remains with each front door.
pub(crate) enum ObjPlacement {
    Affine {
        center: Vec3,
        scale: f64,
    },
    Centered {
        center: Vec3,
        twice_error: Vec3,
        scale: f64,
    },
    Ratio {
        min: Vec3,
        max: Vec3,
        divisor: f64,
        extent: f64,
        height: f64,
    },
}

impl ObjPlacement {
    pub(crate) fn new(vertices: &[Vec3], height: f64) -> Self {
        let Some(&first) = vertices.first() else {
            return Self::Affine {
                center: [0.0; 3],
                scale: 1.0,
            };
        };
        let (mut min, mut max) = (first, first);
        for point in vertices {
            for axis in 0..3 {
                min[axis] = min[axis].min(point[axis]);
                max[axis] = max[axis].max(point[axis]);
            }
        }
        let midpoints: [(f64, f64); 3] =
            std::array::from_fn(|axis| Self::midpoint(min[axis], max[axis]));
        let center = midpoints.map(|(value, _)| value);
        let twice_error = midpoints.map(|(_, error)| error);
        let extent = max[1] - min[1];
        // A heightless source has always been centered without rescaling.
        let scale = if extent == 0.0 { 1.0 } else { height / extent };
        // A nonzero subnormal quotient can already have lost significant
        // relative precision. Multiplying huge source coordinates by it
        // magnifies that quantization into wrong, representable scene geometry.
        // Divide the centered source by its extent before applying height in
        // that case, just as when the quotient underflows completely.
        if extent.is_finite() && (scale.is_normal() || height == 0.0) {
            return if twice_error == [0.0; 3] {
                Self::Affine { center, scale }
            } else {
                Self::Centered {
                    center,
                    twice_error,
                    scale,
                }
            };
        }
        let divisor = if extent.is_finite() {
            extent
        } else {
            min[1].abs().max(max[1].abs())
        };
        Self::Ratio {
            min,
            max,
            divisor,
            extent: Self::difference(max[1], min[1], divisor),
            height,
        }
    }

    /// Return the rounded midpoint and twice its residual. Keeping twice the
    /// residual preserves a half-subnormal until the requested scale is applied.
    fn midpoint(low: f64, high: f64) -> (f64, f64) {
        let sum = low + high;
        let (left, right, sum, half) = if sum.is_finite() {
            (low, high, sum, true)
        } else {
            let left = low * 0.5;
            let right = high * 0.5;
            (left, right, left + right, false)
        };
        // TwoSum retains the part of the endpoint sum that rounding discarded.
        let virtual_right = sum - left;
        let residual = (left - (sum - virtual_right)) + (right - virtual_right);
        let center = if half { sum * 0.5 } else { sum };
        let twice_error = if half {
            residual + (sum - (center + center))
        } else {
            residual + residual
        };
        (center, twice_error)
    }

    fn difference(value: f64, origin: f64, divisor: f64) -> f64 {
        let delta = value - origin;
        if delta.is_finite() {
            delta / divisor
        } else {
            value / divisor - origin / divisor
        }
    }

    pub(crate) fn place(&self, vertex: Vec3) -> Vec3 {
        match *self {
            Self::Affine { center, scale } => {
                std::array::from_fn(|axis| (vertex[axis] - center[axis]) * scale)
            }
            Self::Centered {
                center,
                twice_error,
                scale,
            } => std::array::from_fn(|axis| {
                let offset = vertex[axis] - center[axis];
                let half_error = twice_error[axis] * 0.5;
                if half_error + half_error == twice_error[axis] {
                    (offset - half_error) * scale
                } else {
                    // Halving a subnormal residual may lose geometry that a
                    // usable scale can recover. Scale before halving it here.
                    offset * scale - (twice_error[axis] * scale) * 0.5
                }
            }),
            Self::Ratio {
                min,
                max,
                divisor,
                extent,
                height,
            } => std::array::from_fn(|axis| {
                // Translate before dividing where possible: an unrelated
                // huge coordinate must not swallow a tiny local extent. The
                // scaled midpoint also centers subnormal source coordinates
                // whose unscaled midpoint is not representable.
                let offset = Self::difference(vertex[axis], min[axis], divisor);
                let span = Self::difference(max[axis], min[axis], divisor);
                ((offset - span * 0.5) / extent) * height
            }),
        }
    }
}

/// `ThreeDModel`: an OBJ mesh as a lit, depth-tested triangle-soup mobject
/// (`surface.py:435`). The Reference's `TexturedGeometry` children carry
/// MTL textures; ours converts as a plain-colored mesh — the documented
/// subset limit at the top of this module.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreeDModel {
    mesh: ObjMesh,
    height: f64,
    color: Srgb,
    opacity: f64,
    shading: Vec3,
    depth_test: bool,
    z_index: i32,
}

impl ThreeDModel {
    /// Parse an OBJ model (Reference `ThreeDModel(obj_file)`): Surface-grey,
    /// `height = 3`, shaded `(0.3, 0.2, 0.4)`, depth-tested, centered.
    pub fn from_obj(bytes: &[u8]) -> Result<Self, ObjError> {
        Self::from_obj_with_limits(bytes, &ObjLimits::default())
    }

    /// Parse under explicit budgets (the untrusted-input path).
    pub fn from_obj_with_limits(bytes: &[u8], limits: &ObjLimits) -> Result<Self, ObjError> {
        let mesh = ObjMesh::parse_with_limits(bytes, limits)?;
        Ok(Self {
            mesh,
            height: DEFAULT_MODEL_HEIGHT,
            color: GREY,
            opacity: 1.0,
            shading: MODEL_SHADING,
            depth_test: true,
            z_index: 0,
        })
    }

    /// The scene-space height (`height=`; default 3.0).
    #[must_use]
    pub fn with_height(mut self, height: f64) -> Self {
        self.height = height;
        self
    }

    /// The flat color (`color=`; Surface's GREY default).
    #[must_use]
    pub fn with_color(mut self, color: Srgb, opacity: f64) -> Self {
        self.color = color;
        self.opacity = opacity;
        self
    }

    /// The `(reflectiveness, gloss, shadow)` shading triple.
    #[must_use]
    pub fn with_shading(mut self, shading: Vec3) -> Self {
        self.shading = shading;
        self
    }

    /// Reference `apply_depth_test` / `remove_depth_test`.
    #[must_use]
    pub fn with_depth_test(mut self, depth_test: bool) -> Self {
        self.depth_test = depth_test;
        self
    }

    /// The scene-list sort key (§8.5).
    #[must_use]
    pub fn with_z_index(mut self, z_index: i32) -> Self {
        self.z_index = z_index;
        self
    }

    /// The parsed mesh.
    #[must_use]
    pub fn mesh(&self) -> &ObjMesh {
        &self.mesh
    }
}

impl From<ThreeDModel> for Mobject {
    fn from(model: ThreeDModel) -> Self {
        let placement = ObjPlacement::new(&model.mesh.vertices, model.height);
        // The Reference's Surface dtype, field for field:
        // [('point', f32, 3), ('d_normal_point', f32, 3), ('rgba', f32, 4)]
        // — triangle soup, three consecutive records per triangle.
        let schema = RecordSchema::new(
            &[("point", 3), ("d_normal_point", 3), ("rgba", 4)],
            &["point"],
            &["point", "d_normal_point"],
        )
        .expect("the obj-model record schema is ten lanes");
        // The mesh index buffer is itself memory-resident, so its record
        // count is far below any sizing ceiling.
        let mut buffer = RecordBuffer::new(schema, model.mesh.triangles.len() * 3)
            .expect("obj-model record sizing bounded by the loaded mesh");
        let rgba = [model.color.r, model.color.g, model.color.b, model.opacity];
        for (triangle, corners) in model.mesh.triangles.iter().enumerate() {
            for (k, corner) in corners.iter().enumerate() {
                let point = placement.place(model.mesh.vertices[corner.vertex]);
                // `d_normal_point - point` carries the normal direction;
                // the surface shaders normalize the difference.
                let normal = model.mesh.corner_normal(triangle, k);
                let d_normal_point = [
                    point[0] + normal[0],
                    point[1] + normal[1],
                    point[2] + normal[2],
                ];
                let record = triangle * 3 + k;
                #[allow(clippy::cast_possible_truncation)]
                buffer.write(
                    record,
                    "point",
                    &[point[0] as f32, point[1] as f32, point[2] as f32],
                );
                #[allow(clippy::cast_possible_truncation)]
                buffer.write(
                    record,
                    "d_normal_point",
                    &[
                        d_normal_point[0] as f32,
                        d_normal_point[1] as f32,
                        d_normal_point[2] as f32,
                    ],
                );
                #[allow(clippy::cast_possible_truncation)]
                buffer.write(
                    record,
                    "rgba",
                    &[
                        rgba[0] as f32,
                        rgba[1] as f32,
                        rgba[2] as f32,
                        rgba[3] as f32,
                    ],
                );
            }
        }
        let uniforms = Uniforms {
            shading: model.shading,
            depth_test: model.depth_test,
            ..Uniforms::default()
        };
        Mobject::from_buffer(buffer)
            .with_uniforms(uniforms)
            .with_render_primitive(RenderPrimitive::TriangleMesh)
            .with_z_index(model.z_index)
    }
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    fn close_vec(a: Vec3, b: Vec3) -> bool {
        (0..3).all(|k| (a[k] - b[k]).abs() < 1e-9)
    }

    /// A tetrahedron with per-vertex normals (the unit axes + one diagonal).
    const TETRAHEDRON: &str = "\
# a tetrahedron
v 0.0 1.0 0.0
v -1.0 -1.0 1.0
v 1.0 -1.0 1.0
v 0.0 -1.0 -1.0
vn 1.0 0.0 0.0
vn 0.0 1.0 0.0
vn 0.0 0.0 1.0
vn -1.0 0.0 0.0
f 1//1 2//2 3//3
f 1//1 3//3 4//4
f 1//1 4//4 2//2
f 2//2 4//4 3//3
";

    #[test]
    fn tetrahedron_fixture_counts() {
        let mesh = ObjMesh::parse(TETRAHEDRON.as_bytes()).expect("parses");
        assert_eq!(mesh.vertex_count(), 4);
        assert_eq!(mesh.normals.len(), 4);
        assert_eq!(mesh.triangle_count(), 4);
        assert!(mesh.tex_coords.is_empty());
        // Indices resolve 1-based to 0-based.
        assert_eq!(mesh.triangles[0][0].vertex, 0);
        assert_eq!(mesh.triangles[0][2].vertex, 2);
        assert_eq!(mesh.triangles[0][0].normal, Some(0));
        assert!(mesh.triangles[0][0].tex_coord.is_none());
    }

    #[test]
    fn explicit_normals_are_used_and_normalized() {
        let mesh = ObjMesh::parse(TETRAHEDRON.as_bytes()).expect("parses");
        assert!(close_vec(mesh.corner_normal(0, 0), [1.0, 0.0, 0.0]));
        assert!(close_vec(mesh.corner_normal(0, 2), [0.0, 0.0, 1.0]));
        // A non-unit declared normal is normalized at read time.
        let scaled = "v 0 0 0\nv 1 0 0\nv 0 1 0\nvn 0 0 5\nf 1//1 2//1 3//1\n";
        let mesh = ObjMesh::parse(scaled.as_bytes()).expect("parses");
        assert!(close_vec(mesh.corner_normal(0, 0), [0.0, 0.0, 1.0]));
    }

    #[test]
    fn missing_normals_fall_back_to_face_normals() {
        let obj = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n";
        let mesh = ObjMesh::parse(obj.as_bytes()).expect("parses");
        assert!(close_vec(mesh.face_normal(0), [0.0, 0.0, 1.0]));
        assert!(close_vec(mesh.corner_normal(0, 1), [0.0, 0.0, 1.0]));
        // A degenerate triangle has a defined zero normal, not NaN.
        let degenerate = "v 0 0 0\nv 0 0 0\nv 0 0 0\nf 1 2 3\n";
        let mesh = ObjMesh::parse(degenerate.as_bytes()).expect("parses");
        assert_eq!(mesh.face_normal(0), [0.0; 3]);
    }

    #[test]
    fn face_normals_are_independent_of_obj_units() {
        let reference = ObjMesh::parse(b"v 0 0 0\nv 1 0 0\nv 0 1 1\nf 1 2 3\n")
            .expect("reference mesh")
            .face_normal(0);
        for scale in [1e-9, 1e-200, 1e200, f64::from_bits(1)] {
            let source = format!("v 0 0 0\nv {scale:e} 0 0\nv 0 {scale:e} {scale:e}\nf 1 2 3\n");
            let mesh = ObjMesh::parse(source.as_bytes()).expect("finite scaled mesh");
            assert!(
                close_vec(mesh.face_normal(0), reference),
                "scale {scale:e}: {:?}",
                mesh.face_normal(0)
            );
            assert!(close_vec(mesh.corner_normal(0, 2), reference));
        }
        let spanning = ObjMesh::parse(b"v -1e308 0 0\nv 1e308 0 0\nv 0 1e308 1e308\nf 1 2 3\n")
            .expect("finite vertices despite overflowing edge subtraction");
        assert!(close_vec(spanning.face_normal(0), reference));
    }

    #[test]
    fn oblique_collinear_faces_stay_degenerate_at_finite_scales() {
        for exponent in [-1070, -700, 0, 700, 1017] {
            let scale = if exponent == -1070 {
                f64::from_bits(16)
            } else {
                2.0_f64.powi(exponent)
            };
            assert!(scale > 0.0 && scale.is_finite());
            for offset in [0.0, 64.0, -64.0] {
                for axes in [[0, 1, 2], [1, 2, 0], [2, 0, 1]] {
                    let mut source = String::new();
                    for point in [[0.0, 0.0, 0.0], [1.0, 15.0, 0.0], [3.0, 45.0, 0.0]] {
                        let p = axes.map(|axis| (point[axis] + offset) * scale);
                        source.push_str(&format!("v {:e} {:e} {:e}\n", p[0], p[1], p[2]));
                    }
                    for winding in ["1 2 3", "1 3 2"] {
                        let mesh = ObjMesh::parse(format!("{source}f {winding}\n").as_bytes())
                            .expect("finite exactly collinear vertices");
                        assert_eq!(
                            mesh.face_normal(0),
                            [0.0; 3],
                            "exponent {exponent}, offset {offset}, axes {axes:?}, {winding}"
                        );
                        assert_eq!(mesh.corner_normal(0, 0), [0.0; 3]);
                    }
                }
            }
        }
    }

    #[test]
    fn conditioned_faces_keep_a_small_real_area_and_its_winding() {
        for exponent in [-700, 0, 700] {
            let scale = 2.0_f64.powi(exponent);
            // This is the same oblique line, with its third point moved by
            // one exactly representable binary step. Its determinant is
            // positive, however small the source units or the deviation.
            let deviation = 2.0_f64.powi(-40);
            let source = format!(
                "v 0 0 0\nv {:e} {:e} 0\nv {:e} {:e} 0\n",
                scale,
                15.0 * scale,
                3.0 * scale,
                (45.0 + deviation) * scale,
            );
            for (winding, expected) in [("1 2 3", 1.0), ("1 3 2", -1.0)] {
                let mesh = ObjMesh::parse(format!("{source}f {winding}\n").as_bytes()).unwrap();
                assert_eq!(mesh.face_normal(0), [0.0, 0.0, expected], "{exponent}");
            }
        }
    }

    #[test]
    fn authored_normal_direction_is_independent_of_magnitude() {
        for scale in [1.0, 1e-200, 1e200, f64::from_bits(1)] {
            // The authored normal deliberately differs from the +Z face
            // normal, so mistaking a small normal for zero cannot pass.
            let source = format!("v 0 0 0\nv 1 0 0\nv 0 1 0\nvn {scale:e} 0 0\nf 1//1 2//1 3//1\n");
            let mesh = ObjMesh::parse(source.as_bytes()).expect("finite authored normal");
            assert_eq!(mesh.corner_normal(0, 0), [1.0, 0.0, 0.0], "{scale:e}");
        }
        let zero = ObjMesh::parse(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nvn 0 0 0\nf 1//1 2//1 3//1\n")
            .expect("zero normal retains the face fallback");
        assert_eq!(zero.corner_normal(0, 0), [0.0, 0.0, 1.0]);
    }

    #[test]
    fn skinny_face_normal_survives_underflowing_cross_products() {
        let second = [1e-100, 1e-300, 0.0];
        let third = [1e-100, 1.000_000_000_000_001e-300, 0.0];
        for scale in [1.0, 1e100] {
            let source = format!(
                "v 0 0 0\nv {:e} {:e} 0\nv {:e} {:e} 0\nf 1 2 3\n",
                second[0] * scale,
                second[1] * scale,
                third[0] * scale,
                third[1] * scale,
            );
            let mesh = ObjMesh::parse(source.as_bytes()).unwrap();
            // The third vertex is strictly above the second in the XY plane.
            // Multiplying source units cannot turn this face into zero area.
            assert_eq!(mesh.face_normal(0), [0.0, 0.0, 1.0], "{scale:e}");
        }
    }

    #[test]
    fn finite_cross_direction_survives_overflowing_or_underflowing_length() {
        for (wide, narrow) in [(1e300, 1e-100), (1e100, 1e-300)] {
            let source = format!("v 0 0 0\nv {wide:e} 0 0\nv {wide:e} {narrow:e} 0\nf 1 2 3\n");
            let mesh = ObjMesh::parse(source.as_bytes()).unwrap();
            // Each finite cross has a +Z direction, even though squaring its
            // length over/underflows and dividing edges by `wide` loses Y.
            assert_eq!(mesh.face_normal(0), [0.0, 0.0, 1.0], "{wide:e}");
            assert_eq!(mesh.corner_normal(0, 2), [0.0, 0.0, 1.0], "{wide:e}");
        }
    }

    #[test]
    fn negative_indices_are_relative() {
        let obj = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf -3 -2 -1\n";
        let mesh = ObjMesh::parse(obj.as_bytes()).expect("parses");
        assert_eq!(mesh.triangles[0][0].vertex, 0);
        assert_eq!(mesh.triangles[0][1].vertex, 1);
        assert_eq!(mesh.triangles[0][2].vertex, 2);
    }

    #[test]
    fn polygons_fan_triangulate() {
        let quad = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nf 1/1 2/2 3/3 4/4\n";
        let mesh = ObjMesh::parse(quad.as_bytes()).expect("parses");
        assert_eq!(mesh.triangle_count(), 2);
        assert_eq!(mesh.triangles[0][0].vertex, 0);
        assert_eq!(mesh.triangles[1][0].vertex, 0, "fan root repeats");
        assert_eq!(mesh.triangles[1][1].vertex, 2);
        assert_eq!(mesh.triangles[1][2].vertex, 3);
        assert_eq!(mesh.triangles[1][2].tex_coord, Some(3));
    }

    #[test]
    fn malformed_inputs_name_their_errors() {
        // A non-finite coordinate ("nan" parses as f64 — must still refuse).
        let err = ObjMesh::parse(b"v 0 nan 0\n").expect_err("refused");
        assert!(matches!(
            err,
            ObjError::BadFloat {
                line: 1,
                statement: "v",
                ..
            }
        ));
        // A short vertex.
        let err = ObjMesh::parse(b"v 0 1\n").expect_err("refused");
        assert!(matches!(err, ObjError::BadFloat { line: 1, .. }));
        // A non-integer index.
        let err = ObjMesh::parse(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 two 3\n").expect_err("refused");
        assert!(matches!(err, ObjError::BadIndex { line: 4, .. }));
        // Zero index (OBJ is 1-based).
        let err = ObjMesh::parse(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 0 3\n").expect_err("refused");
        assert!(matches!(
            err,
            ObjError::IndexOutOfRange {
                line: 4,
                index: 0,
                len: 3
            }
        ));
        // Past-the-end index.
        let err = ObjMesh::parse(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 9\n").expect_err("refused");
        assert!(matches!(err, ObjError::IndexOutOfRange { index: 9, .. }));
        // A two-corner face.
        let err = ObjMesh::parse(b"v 0 0 0\nv 1 0 0\nf 1 2\n").expect_err("refused");
        assert_eq!(
            err,
            ObjError::FaceTooSmall {
                line: 3,
                corners: 2
            }
        );
        // Invalid UTF-8 names the byte offset.
        let err = ObjMesh::parse(b"v 0 0 \xff\n").expect_err("refused");
        assert!(matches!(err, ObjError::NotUtf8 { .. }));
        // Every error text is precise (the fuzz campaign's bar).
        for input in [
            &b"v 0 nan 0\n"[..],
            b"v 0 1\n",
            b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 two 3\n",
            b"v 0 0 0\nv 1 0 0\nf 1 2\n",
            b"v 0 0 \xff\n",
        ] {
            let err = ObjMesh::parse(input).expect_err("refused");
            let message = err.to_string();
            assert!(!message.is_empty());
            assert!(message.contains("line") || message.contains("offset"));
        }
    }

    #[test]
    fn budgets_refuse_with_named_errors() {
        let limits = ObjLimits {
            max_vertices: 2,
            ..ObjLimits::default()
        };
        let err = ObjMesh::parse_with_limits(b"v 0 0 0\nv 1 0 0\nv 0 1 0\n", &limits)
            .expect_err("refused");
        assert_eq!(err, ObjError::TooManyVertices { line: 3, limit: 2 });
        let limits = ObjLimits {
            max_triangles: 1,
            ..ObjLimits::default()
        };
        let quad = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\n";
        let err = ObjMesh::parse_with_limits(quad.as_bytes(), &limits).expect_err("refused");
        assert_eq!(err, ObjError::TooManyTriangles { line: 5, limit: 1 });
    }

    #[test]
    fn skipped_statements_are_tolerated() {
        let obj = "o thing\ng group\ns off\nusemtl mat\nmtllib lib.mtl\nl 1 2\np 1\nvp 0.5\n\
                   cstype rat bspline\ncurv 0 1 1 2\n\
                   v 0 0 0 1.0\nv 1 0 0\nv 0 1 0\nvt 0.5\nf 1/1 2 3\n";
        let mesh = ObjMesh::parse(obj.as_bytes()).expect("parses");
        assert_eq!(mesh.vertex_count(), 3);
        assert_eq!(mesh.tex_coords, vec![[0.5, 0.0]]);
        assert_eq!(mesh.triangle_count(), 1);
    }

    #[test]
    fn crlf_and_comments_are_handled() {
        let obj = "v 0 0 0 # trailing comment\r\nv 1 0 0\r\nv 0 1 0\r\n# full line\r\nf 1 2 3\r\n";
        let mesh = ObjMesh::parse(obj.as_bytes()).expect("parses");
        assert_eq!(mesh.triangle_count(), 1);
    }

    #[test]
    fn three_d_model_normalizes_height_and_centers() {
        let model = ThreeDModel::from_obj(TETRAHEDRON.as_bytes()).expect("parses");
        let mob = Mobject::from(model);
        // 4 triangles × 3 corners of the Surface dtype.
        assert_eq!(mob.buffer.len(), 12);
        let names: Vec<&str> = mob
            .buffer
            .schema()
            .fields()
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(names, ["point", "d_normal_point", "rgba"]);
        // Height normalization: the tetrahedron's y-extent is 2, so the
        // scaled extent is the default model height of 3.
        let points = mob.buffer.read_column("point").expect("field");
        let ys: Vec<f32> = points.iter().skip(1).step_by(3).copied().collect();
        let (min, max) = ys
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &y| {
                (lo.min(y), hi.max(y))
            });
        assert!((f64::from(max - min) - DEFAULT_MODEL_HEIGHT).abs() < 1e-5);
        // Centered: the bbox midpoint lands on the origin.
        assert!((f64::from(max + min) / 2.0).abs() < 1e-5);
        // Normals ride the d_normal_point offset (first corner: vn 1,0,0).
        let point = mob.buffer.read(0, "point").expect("field");
        let dnp = mob.buffer.read(0, "d_normal_point").expect("field");
        assert!((dnp[0] - point[0] - 1.0).abs() < 1e-5);
        assert!((dnp[1] - point[1]).abs() < 1e-5);
        // Surface shading and depth test land in the uniform inventory.
        assert_eq!(mob.uniforms.shading, MODEL_SHADING);
        assert!(mob.uniforms.depth_test);
    }

    #[test]
    fn three_d_model_normalizes_finite_extreme_scales() {
        let reference = Mobject::from(
            ThreeDModel::from_obj(b"v -1 -1 0\nv 1 -1 0\nv 0 1 0\nf 1 2 3\n")
                .expect("reference model"),
        );
        for scale in [1e-200, 1e200, f64::from_bits(1), f64::MAX] {
            let source = format!(
                "v -{scale:e} -{scale:e} 0\nv {scale:e} -{scale:e} 0\nv 0 {scale:e} 0\nf 1 2 3\n"
            );
            let model = Mobject::from(
                ThreeDModel::from_obj(source.as_bytes()).expect("finite scaled model"),
            );
            for field in ["point", "d_normal_point"] {
                assert_eq!(
                    model.buffer.read_column(field).expect("model field"),
                    reference
                        .buffer
                        .read_column(field)
                        .expect("reference field"),
                    "scale {scale:e}, {field}"
                );
            }
        }
    }

    #[test]
    fn model_normalization_keeps_small_extents_beside_large_translations() {
        let reference = Mobject::from(
            ThreeDModel::from_obj(b"v 0 0 0\nv 0 1 0\nv 0 0 1\nf 1 2 3\n")
                .expect("reference model"),
        );
        let translated = Mobject::from(
            ThreeDModel::from_obj(b"v 1e308 0 0\nv 1e308 1 0\nv 1e308 0 1\nf 1 2 3\n")
                .expect("translated finite model"),
        );
        for field in ["point", "d_normal_point"] {
            assert_eq!(
                translated.buffer.read_column(field).expect("model field"),
                reference
                    .buffer
                    .read_column(field)
                    .expect("reference field"),
                "{field}"
            );
        }
    }

    #[test]
    fn model_normalization_does_not_quantize_subnormal_scale_ratios() {
        for height in [
            3.0,
            1e-10,
            1e-13,
            1e-15,
            1e-17,
            f64::from(f32::MIN_POSITIVE),
            f64::from(f32::from_bits(16)),
            -1e-15,
            0.0,
        ] {
            #[allow(clippy::cast_possible_truncation)]
            let half = (height * 0.5) as f32;
            let expected = vec![-half, -half, 0.0, half, -half, 0.0, -half, half, 0.0];
            let reference = Mobject::from(
                ThreeDModel::from_obj(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n")
                    .unwrap()
                    .with_height(height),
            );
            for scale in [1.0, 1e100, 1e200, 1e308] {
                let source = format!("v 0 0 0\nv {scale:e} 0 0\nv 0 {scale:e} 0\nf 1 2 3\n");
                let model = Mobject::from(
                    ThreeDModel::from_obj(source.as_bytes())
                        .unwrap()
                        .with_height(height),
                );
                assert_eq!(
                    model.buffer.read_column("point").unwrap(),
                    expected,
                    "source units {scale:e}, requested height {height:e}"
                );
                assert_eq!(
                    model.buffer.read_column("d_normal_point"),
                    reference.buffer.read_column("d_normal_point"),
                    "source units {scale:e}, requested height {height:e}"
                );
            }
        }
    }

    #[test]
    fn model_normalization_centers_unrepresentable_midpoints() {
        for (low, side) in [
            ([1e16; 3], 2.0),
            ([-1e16; 3], 2.0),
            ([1e16, -1e16, 1e16], 2.0),
            ([0.0; 3], f64::from_bits(1)),
        ] {
            let [x0, y0, z0] = low;
            let [x1, y1, z1] = low.map(|value| value + side);
            let source = format!(
                "v {x0:e} {y0:e} {z0:e}\nv {x1:e} {y0:e} {z1:e}\nv {x0:e} {y1:e} {z0:e}\nf 1 2 3\n"
            );
            for height in [DEFAULT_MODEL_HEIGHT, 7.5, 1e-17] {
                let build = |source: &[u8]| {
                    let model = ThreeDModel::from_obj(source).expect("finite source geometry");
                    Mobject::from(if height == DEFAULT_MODEL_HEIGHT {
                        model
                    } else {
                        model.with_height(height)
                    })
                };
                let model = build(source.as_bytes());
                #[allow(clippy::cast_possible_truncation)]
                let half = (height * 0.5) as f32;
                assert_eq!(
                    model.buffer.read_column("point").unwrap(),
                    vec![-half, -half, -half, half, -half, half, -half, half, -half],
                    "{low:?}, side {side:e}, height {height:e}"
                );
                let reference = build(b"v 0 0 0\nv 2 0 2\nv 0 2 0\nf 1 2 3\n");
                assert_eq!(
                    model.buffer.read_column("d_normal_point"),
                    reference.buffer.read_column("d_normal_point"),
                    "{low:?}, side {side:e}, height {height:e}"
                );
            }
        }
    }

    #[test]
    fn model_placement_preserves_wide_axes_at_subnormal_heights() {
        let tiny = f64::from_bits(1);
        let vertices = [[1e16, 0.0, 0.0], [1e16 + 2.0, tiny, 0.0]];
        let placement = ObjPlacement::new(&vertices, tiny);
        for (vertex, expected_x) in vertices.into_iter().zip([-1.0, 1.0]) {
            let point = placement.place(vertex);
            assert!(point.into_iter().all(f64::is_finite));
            assert_eq!(point[0], expected_x);
        }
        // The exact center is 5e15 + 0.5. Normalizing the whole X range
        // before subtracting its midpoint would erase this interior offset.
        let interior = [[1.0, 0.0, 0.0], [1e16, tiny, 0.0], [5e15, 0.0, 0.0]];
        let placement = ObjPlacement::new(&interior, tiny);
        assert_eq!(placement.place(interior[2])[0], -0.5);
        let zero = ObjPlacement::new(&vertices, 0.0);
        for vertex in vertices {
            assert_eq!(zero.place(vertex), [0.0; 3]);
        }
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn model_placement_corrects_midpoints_without_overflowing_source_arithmetic() {
        let high = 2.0_f64.powi(970);
        let vertices = [[-f64::MAX, 0.0, 0.0], [high, 1e-20, 0.0]];
        assert!((high - vertices[0][0]).is_infinite());
        let placement = ObjPlacement::new(&vertices, 1e-300);
        let half_width = (f64::MAX * 0.5 + high * 0.5) * (1e-300 / 1e-20);
        for (vertex, sign) in vertices.into_iter().zip([-1.0, 1.0]) {
            let point = placement.place(vertex);
            assert!(point.into_iter().all(f64::is_finite));
            assert_eq!(point[0] as f32, (sign * half_width) as f32);
        }
        // Their sum overflows and their exact midpoint lies halfway between
        // adjacent finite floats, so the sum-of-halves residual matters too.
        let low = f64::from_bits(f64::MAX.to_bits() - 1);
        let vertices = [[low, 0.0, 0.0], [f64::MAX, 1e300, 0.0]];
        let placement = ObjPlacement::new(&vertices, 3.0);
        let half_width = (f64::MAX - low) * 0.5 * (3.0 / 1e300);
        for (vertex, sign) in vertices.into_iter().zip([-1.0, 1.0]) {
            let point = placement.place(vertex);
            assert!(point.into_iter().all(f64::is_finite));
            assert_eq!(point[0] as f32, (sign * half_width) as f32);
        }
    }

    // ---- structure-aware fuzz: the parser never panics -------------------

    /// A small deterministic PRNG for the fuzz-style tests (xorshift64;
    /// fixed seed — no wall clock, no entropy).
    struct XorShift64(u64);

    impl XorShift64 {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Perturb a valid OBJ document in a structure-aware way: digit swaps
    /// in coordinates, index inflation/deflation, token corruption, line
    /// duplication, truncation, and byte flips.
    fn mutate(rng: &mut XorShift64, input: &mut Vec<u8>) {
        match rng.below(8) {
            0 => {
                // Digit perturbation: rewrite a random ASCII digit.
                let digits: Vec<usize> = input
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| b.is_ascii_digit())
                    .map(|(i, _)| i)
                    .collect();
                if !digits.is_empty() {
                    let at = digits[rng.below(digits.len() as u64) as usize];
                    input[at] = b'0' + (rng.below(10) as u8);
                }
            }
            1 => {
                // Truncate at a random line boundary (byte 10 is '\n').
                let lines: Vec<usize> = input
                    .iter()
                    .enumerate()
                    .filter(|&(_, b)| *b == 10)
                    .map(|(i, _)| i)
                    .collect();
                if !lines.is_empty() {
                    input.truncate(lines[rng.below(lines.len() as u64) as usize]);
                }
            }
            2 => {
                // Duplicate a random line.
                let text = String::from_utf8_lossy(input).into_owned();
                let lines: Vec<&str> = text.lines().collect();
                if !lines.is_empty() {
                    let line = lines[rng.below(lines.len() as u64) as usize];
                    let at = rng.below(input.len() as u64) as usize;
                    let mut insertion = line.as_bytes().to_vec();
                    insertion.push(b'\n');
                    input.splice(at..at, insertion);
                }
            }
            3 => {
                // Flip a random byte.
                if !input.is_empty() {
                    let at = rng.below(input.len() as u64) as usize;
                    input[at] ^= 1u8 << (rng.below(8) as u8);
                }
            }
            4 => {
                // Corrupt a random statement keyword.
                let statements: Vec<usize> = input
                    .windows(2)
                    .enumerate()
                    .filter(|(_, w)| matches!(*w, b"v " | b"f " | b"vn" | b"vt"))
                    .map(|(i, _)| i)
                    .collect();
                if !statements.is_empty() {
                    let at = statements[rng.below(statements.len() as u64) as usize];
                    input[at] = b'a' + (rng.below(26) as u8);
                }
            }
            5 => {
                // Splice a random chunk.
                if input.len() > 4 {
                    let at = rng.below((input.len() - 2) as u64) as usize;
                    let len = 1 + rng.below(4) as usize;
                    let end = (at + len).min(input.len());
                    input.splice(at..end, [b'/'; 1]);
                }
            }
            6 => {
                // Sign flip on the first '-' (0x2D).
                if let Some(at) = input.iter().position(|&b| matches!(b, 0x2D)) {
                    input[at] = b'+';
                }
            }
            _ => {
                // Inject a malformed statement line.
                let injection: &[u8] = match rng.below(4) {
                    0 => b"v nan 0 0\n",
                    1 => b"f 0 0 0\n",
                    2 => b"f 1/2/3/4 1 1\n",
                    _ => b"v 1 2\n",
                };
                let at = rng.below(input.len() as u64 + 1) as usize;
                input.splice(at..at, injection.iter().copied());
            }
        }
    }

    #[test]
    fn obj_parser_never_panics_under_structure_aware_mutation() {
        const CASES: u32 = 2_000;
        let seeds: [&str; 3] = [
            TETRAHEDRON,
            "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nf 1/1 2/2 3/3 4/4\n",
            "v 0 0 0\nv 1 0 0\nv 0 1 0\nf -3 -2 -1\n",
        ];
        let mut rng = XorShift64(0x0b1e_c7a5_5eed_0001);
        for case in 0..CASES {
            let mut input = seeds[rng.below(seeds.len() as u64) as usize]
                .as_bytes()
                .to_vec();
            let mutations = 1 + rng.below(6);
            for _ in 0..mutations {
                mutate(&mut rng, &mut input);
            }
            // Totality: every input is a typed verdict, never a panic
            // (a panic here fails the test by unwinding).
            let verdict = ObjMesh::parse_with_limits(
                &input,
                &ObjLimits {
                    max_vertices: 1 << 12,
                    max_tex_coords: 1 << 12,
                    max_normals: 1 << 12,
                    max_triangles: 1 << 13,
                },
            );
            match verdict {
                Ok(mesh) => {
                    // Accepted parses honor the budgets they were run under.
                    assert!(
                        mesh.vertex_count() <= 1 << 12,
                        "case {case}: budget breached"
                    );
                    assert!(mesh.triangle_count() <= 1 << 13);
                    // Every referenced index is in range by construction.
                    for triangle in &mesh.triangles {
                        for corner in triangle {
                            assert!(corner.vertex < mesh.vertex_count());
                            if let Some(n) = corner.normal {
                                assert!(n < mesh.normals.len());
                            }
                            if let Some(t) = corner.tex_coord {
                                assert!(t < mesh.tex_coords.len());
                            }
                        }
                    }
                }
                Err(err) => {
                    // Refusals are precise: non-empty and line-locating
                    // (the fuzz campaign's precision bar).
                    let message = err.to_string();
                    assert!(!message.is_empty(), "case {case}: empty error");
                    assert!(
                        message.contains("line") || message.contains("offset"),
                        "case {case}: imprecise error {message:?}"
                    );
                }
            }
        }
    }
}
