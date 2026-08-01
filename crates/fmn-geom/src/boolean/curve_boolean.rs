//! The `transversal-interiors` (C1) curve-aware boolean class.
//!
//! The normative specification is `docs/geometry/curve_booleans.md`; this
//! module is its executable form. Every cross-operand piece intersection
//! must be a proper crossing strictly inside both pieces (spec §3.4),
//! every same-operand pair must meet only at shared contour endpoints,
//! and every numerical screen below is a *routing* decision: anything
//! unresolved, singular, tangent, endpoint-touching, overlapping, or
//! budget-exhausting declines the class and the caller falls through to
//! the certified flatten-and-clip Stage 1. Declining never fails the
//! operation and never coarsens output silently.
//!
//! Pipeline: piece collection (with Stage-1's closure snap) → fat-line
//! pruned Bézier clipping (bounded rounds, bounded subdivision) → bounded
//! Newton polish → transversal-interior screening → de Casteljau
//! splitting with shared-vertex snap → winding classification by ray
//! casting → the same angular-sort face traversal as Stage 1, stitched
//! over curve tangents. Nothing here claims coordinate exactness; the
//! double-double orientation and area signs shared with Stage 1 are used
//! so routing signs agree with the arrangement's own predicates.

use super::{
    BooleanOperation, BooleanOptions, BooleanResult, BooleanRoute, BooleanStats, Dd, add,
    angle_cmp, coordinate_resolution, cross_dd, dot, has_nonzero_anchor_area, length, lerp,
    midpoint, operation_value, point_cmp, point_segment_distance, product_difference, scale, sub,
    to_vec3, xy,
};
use crate::QuadPath;

type Point = [f64; 2];

/// Open-interval margin for a transversal interior crossing (spec §3.4).
const INTERIOR_MARGIN: f64 = 1.0e-9;
/// Minimum crossing sine for a transversal crossing (spec §3.4).
const SINE_MIN: f64 = 1.0e-6;
/// Clip-interval width that counts as convergence (spec §3.2).
const CLIP_T_EPS: f64 = 1.0e-10;
/// Two roots on one piece closer than this are a near-double root (C3).
const ROOT_MERGE_EPS: f64 = 1.0e-9;
/// Documented clip-round bound per recursion node (spec §3.2).
const MAX_CLIP_ROUNDS: u32 = 24;
/// Documented bisection depth bound (spec §3.2).
const MAX_CLIP_DEPTH: u32 = 6;
/// Documented clip-node bound per piece pair (spec §3.2).
const MAX_CLIP_NODES: u32 = 512;
/// Documented Newton iteration bound (spec §3.3).
const MAX_NEWTON_ITERATIONS: u32 = 16;
/// Jacobian singularity threshold relative to derivative magnitudes (§3.3).
const JACOBIAN_SINGULAR: f64 = 1.0e-13;
/// Ray-crossing parameters within this of an endpoint make the sample ray
/// ambiguous; classification retries with a smaller offset (spec §4).
const RAY_PARAM_SNAP: f64 = 1.0e-9;
/// Classification sample retries with halved offsets (spec §4).
const SAMPLE_ATTEMPTS: u32 = 4;

/// One quadratic piece of an operand contour.
#[derive(Clone, Copy)]
struct Piece {
    /// 0 for the subject, 1 for the clip.
    operand: usize,
    quad: Quad,
    /// Vertex of the piece start anchor.
    start_vertex: usize,
    /// Vertex of the piece end anchor.
    end_vertex: usize,
    /// Whether the handle lies off the chord (a genuine curve).
    curved: bool,
}

/// A quadratic Bézier in the plane.
#[derive(Clone, Copy)]
struct Quad {
    p0: Point,
    p1: Point,
    p2: Point,
}

impl Quad {
    fn eval(self, t: f64) -> Point {
        let mt = 1.0 - t;
        add(
            add(scale(self.p0, mt * mt), scale(self.p1, 2.0 * mt * t)),
            scale(self.p2, t * t),
        )
    }

    fn deriv(self, t: f64) -> Point {
        let mt = 1.0 - t;
        add(
            scale(sub(self.p1, self.p0), 2.0 * mt),
            scale(sub(self.p2, self.p1), 2.0 * t),
        )
    }

    /// De Casteljau split at `t`: exact in the parameterization (spec §3.6).
    fn split(self, t: f64) -> (Self, Self) {
        let q0 = lerp(self.p0, self.p1, t);
        let q1 = lerp(self.p1, self.p2, t);
        let r = lerp(q0, q1, t);
        (
            Self {
                p0: self.p0,
                p1: q0,
                p2: r,
            },
            Self {
                p0: r,
                p1: q1,
                p2: self.p2,
            },
        )
    }

    /// The subcurve over `[a, b]` of the parameter interval.
    fn subcurve(self, a: f64, b: f64) -> Self {
        if b - a >= 1.0 {
            return self;
        }
        let (left, _) = self.split(b.clamp(0.0, 1.0));
        if b <= 0.0 {
            return left;
        }
        let (_, right) = left.split((a / b).clamp(0.0, 1.0));
        right
    }

    /// The tight quadratic fat line (spec §3.1): baseline point, unit
    /// normal, and the signed-distance band containing the curve.
    fn fat_line(self) -> (Point, Point, f64, f64) {
        let chord = sub(self.p2, self.p0);
        let chord_length = length(chord);
        if chord_length == 0.0 {
            return (self.p0, [0.0, 1.0], 0.0, 0.0);
        }
        let normal = [-chord[1] / chord_length, chord[0] / chord_length];
        let d1 = dot(normal, sub(self.p1, self.p0)) * 0.5;
        (self.p0, normal, d1.min(0.0), d1.max(0.0))
    }

    /// Signed area via the exact quadratic boundary integral (spec §5),
    /// scaled by 6: `2·cross(p0,p1) + cross(p0,p2) + 2·cross(p1,p2)`.
    fn area_times_six(self) -> Dd {
        let c01 = product_difference(self.p0[0], self.p1[1], self.p0[1], self.p1[0]);
        let c02 = product_difference(self.p0[0], self.p2[1], self.p0[1], self.p2[0]);
        let c12 = product_difference(self.p1[0], self.p2[1], self.p1[1], self.p2[0]);
        c01.add(c01).add(c02).add(c12).add(c12)
    }
}

/// Attempt the `transversal-interiors` class. `None` declines the class;
/// the caller then runs the certified fallback. Every refusal path is a
/// routing decision, never an error and never a coarsened result.
pub(super) fn try_transversal(
    subject: &QuadPath,
    clip: &QuadPath,
    operation: BooleanOperation,
    options: BooleanOptions,
) -> Option<BooleanResult> {
    if !has_nonzero_anchor_area(subject) || !has_nonzero_anchor_area(clip) {
        return None;
    }
    let mut vertices: Vec<Point> = Vec::new();
    let mut pieces: Vec<Piece> = Vec::new();
    for (operand, path) in [(0_usize, subject), (1_usize, clip)] {
        collect_operand(path, operand, options, &mut vertices, &mut pieces)?;
    }
    if !pieces.iter().any(|piece| piece.curved) {
        // All-line inputs are exact under Stage 1 and stay there (spec §1).
        return None;
    }
    let scene_scale = pieces
        .iter()
        .flat_map(|piece| [piece.quad.p0, piece.quad.p1, piece.quad.p2])
        .fold(1.0_f64, |acc, p| acc.max(p[0].abs()).max(p[1].abs()));

    let mut stats = BooleanStats {
        input_curves: subject.num_curves().saturating_add(clip.num_curves()),
        ..BooleanStats::default()
    };
    let dbg = std::env::var_os("FMN_CURVE_DEBUG").is_some();
    let Some(intersections) =
        solve_intersections(&pieces, scene_scale, options, &mut stats, &mut vertices)
    else {
        if dbg {
            eprintln!("decline: solve_intersections (pieces={})", pieces.len());
        }
        return None;
    };
    let Some(edges) = split_into_atomic_edges(&pieces, &intersections, &vertices) else {
        if dbg {
            eprintln!("decline: split (roots={})", intersections.len());
        }
        return None;
    };
    let Some(graph) = classify(&vertices, &edges, operation, options, &mut stats) else {
        if dbg {
            eprintln!("decline: classify (edges={})", edges.len());
        }
        return None;
    };
    let Some(loops) = graph.trace_loops(options, &mut stats) else {
        if dbg {
            eprintln!("decline: trace");
        }
        return None;
    };
    let Some(path) = emit_path(&loops) else {
        if dbg {
            eprintln!("decline: emit");
        }
        return None;
    };

    stats.output_contours = loops.len();
    Some(BooleanResult {
        path,
        route: BooleanRoute::CurveAwareTransversal,
        stats,
    })
}

/// Collect an operand's contours into pieces and anchor vertices,
/// applying the closure snap and every C5 degenerate-piece refusal.
fn collect_operand(
    path: &QuadPath,
    operand: usize,
    options: BooleanOptions,
    vertices: &mut Vec<Point>,
    pieces: &mut Vec<Piece>,
) -> Option<()> {
    let operand_start = pieces.len();
    for subpath in path.subpaths() {
        if subpath.len() < 3 {
            continue;
        }
        let anchors: Vec<Point> = subpath.iter().step_by(2).map(|&p| xy(p)).collect();
        let handles: Vec<Point> = subpath.iter().skip(1).step_by(2).map(|&p| xy(p)).collect();
        let last = anchors.len() - 1;
        let snapped = length(sub(anchors[last], anchors[0])) <= options.tolerance;
        let cycle_len = if snapped { last } else { anchors.len() };
        if cycle_len == 0 {
            return None;
        }
        let cycle: Vec<usize> = (0..cycle_len)
            .map(|index| {
                vertices.push(anchors[index]);
                vertices.len() - 1
            })
            .collect();
        for index in 0..last {
            let end = (index + 1) % cycle_len;
            push_piece(
                operand,
                Quad {
                    p0: anchors[index],
                    p1: handles[index],
                    p2: if end == 0 {
                        anchors[0]
                    } else {
                        anchors[index + 1]
                    },
                },
                cycle[index],
                cycle[end],
                pieces,
            )?;
        }
        if !snapped {
            push_piece(
                operand,
                Quad {
                    p0: anchors[last],
                    p1: midpoint(anchors[last], anchors[0]),
                    p2: anchors[0],
                },
                cycle[last],
                cycle[0],
                pieces,
            )?;
        }
    }
    // Pinch points and self-touching contours are C2/C5: within one
    // operand every anchor must be distinct (the closure identification
    // is already folded into the cycle).
    let mut starts: Vec<Point> = pieces[operand_start..]
        .iter()
        .map(|piece| piece.quad.p0)
        .collect();
    starts.sort_by(|a, b| point_cmp(*a, *b));
    if starts.windows(2).any(|pair| pair[0] == pair[1]) {
        return None;
    }
    Some(())
}

fn push_piece(
    operand: usize,
    quad: Quad,
    start_vertex: usize,
    end_vertex: usize,
    pieces: &mut Vec<Piece>,
) -> Option<()> {
    if quad.p0 == quad.p1 || quad.p1 == quad.p2 || quad.p0 == quad.p2 {
        return None;
    }
    pieces.push(Piece {
        operand,
        quad,
        start_vertex,
        end_vertex,
        curved: cross_dd(sub(quad.p1, quad.p0), sub(quad.p2, quad.p0)).sign() != 0,
    });
    Some(())
}

/// One proved transversal interior crossing, shared by both pieces.
#[derive(Clone, Copy)]
struct Intersection {
    piece_a: usize,
    t_a: f64,
    piece_b: usize,
    t_b: f64,
    vertex: usize,
}

/// Solve every piece pair: same-operand pairs must meet only at shared
/// endpoints; cross-operand roots must all pass the C1 screen. Any
/// solver failure, tangency, endpoint touch, overlap, or budget
/// exhaustion declines the class.
fn solve_intersections(
    pieces: &[Piece],
    scene_scale: f64,
    options: BooleanOptions,
    stats: &mut BooleanStats,
    vertices: &mut Vec<Point>,
) -> Option<Vec<Intersection>> {
    let mut intersections = Vec::new();
    for i in 0..pieces.len() {
        for j in i + 1..pieces.len() {
            if !boxes_overlap(pieces[i].quad, pieces[j].quad) {
                continue;
            }
            bump(&mut stats.pair_tests, options.limits.max_pair_tests)?;
            let dbg = std::env::var_os("FMN_CURVE_DEBUG").is_some();
            let Some(roots) = clip_pair(&pieces[i].quad, &pieces[j].quad, options, stats) else {
                if dbg {
                    eprintln!(
                        "  clip_pair None: pieces ({i},{j}) same_op={} curved=({},{})",
                        pieces[i].operand == pieces[j].operand,
                        pieces[i].curved,
                        pieces[j].curved
                    );
                }
                return None;
            };
            if pieces[i].operand == pieces[j].operand {
                if roots
                    .iter()
                    .any(|&(t, u)| !at_shared_endpoint(&pieces[i].quad, &pieces[j].quad, t, u))
                {
                    if dbg {
                        eprintln!("  same-operand non-endpoint root: pieces ({i},{j}) roots={roots:?}");
                    }
                    return None;
                }
                continue;
            }
            for (t_seed, u_seed) in roots {
                let (t, u) = polish(
                    &pieces[i].quad,
                    &pieces[j].quad,
                    t_seed,
                    u_seed,
                    scene_scale,
                )?;
                if !is_transversal_interior(&pieces[i].quad, &pieces[j].quad, t, u) {
                    return None;
                }
                bump(&mut stats.intersections, options.limits.max_intersections)?;
                vertices.push(midpoint(pieces[i].quad.eval(t), pieces[j].quad.eval(u)));
                intersections.push(Intersection {
                    piece_a: i,
                    t_a: t,
                    piece_b: j,
                    t_b: u,
                    vertex: vertices.len() - 1,
                });
            }
        }
    }
    // A near-double root on one piece is C3 territory: any two roots on
    // the same piece closer than the merge band decline the class.
    for piece in 0..pieces.len() {
        let mut params: Vec<f64> = intersections
            .iter()
            .filter_map(|hit| {
                if hit.piece_a == piece {
                    Some(hit.t_a)
                } else if hit.piece_b == piece {
                    Some(hit.t_b)
                } else {
                    None
                }
            })
            .collect();
        params.sort_by(f64::total_cmp);
        if params
            .windows(2)
            .any(|pair| pair[1] - pair[0] < ROOT_MERGE_EPS)
        {
            return None;
        }
    }
    Some(intersections)
}

fn boxes_overlap(a: Quad, b: Quad) -> bool {
    let (a_min_x, a_max_x, a_min_y, a_max_y) = quad_box(a);
    let (b_min_x, b_max_x, b_min_y, b_max_y) = quad_box(b);
    a_min_x <= b_max_x && b_min_x <= a_max_x && a_min_y <= b_max_y && b_min_y <= a_max_y
}

fn quad_box(q: Quad) -> (f64, f64, f64, f64) {
    (
        q.p0[0].min(q.p1[0]).min(q.p2[0]),
        q.p0[0].max(q.p1[0]).max(q.p2[0]),
        q.p0[1].min(q.p1[1]).min(q.p2[1]),
        q.p0[1].max(q.p1[1]).max(q.p2[1]),
    )
}

/// Every root of a same-operand pair must sit at an endpoint the two
/// pieces share (a contour joint).
fn at_shared_endpoint(a: &Quad, b: &Quad, t: f64, u: f64) -> bool {
    [(0.0, a.p0), (1.0, a.p2)].iter().any(|&(ta, pa)| {
        [(0.0, b.p0), (1.0, b.p2)].iter().any(|&(tb, pb)| {
            pa == pb && (t - ta).abs() <= INTERIOR_MARGIN && (u - tb).abs() <= INTERIOR_MARGIN
        })
    })
}

/// The C1 acceptance predicate (spec §3.4): strict interior parameters,
/// nonzero derivatives, crossing sine bounded away from zero.
fn is_transversal_interior(a: &Quad, b: &Quad, t: f64, u: f64) -> bool {
    if t < INTERIOR_MARGIN
        || t > 1.0 - INTERIOR_MARGIN
        || u < INTERIOR_MARGIN
        || u > 1.0 - INTERIOR_MARGIN
    {
        return false;
    }
    let da = a.deriv(t);
    let db = b.deriv(u);
    let la = length(da);
    let lb = length(db);
    if la == 0.0 || lb == 0.0 {
        return false;
    }
    cross_dd(da, db).value().abs() >= SINE_MIN * la * lb
}

/// Fat-line pruned Bézier clipping (spec §3.1, §3.2): bounded rounds,
/// bounded bisection, bounded nodes. `None` means unresolved, which
/// declines the class.
fn clip_pair(
    a: &Quad,
    b: &Quad,
    options: BooleanOptions,
    stats: &mut BooleanStats,
) -> Option<Vec<(f64, f64)>> {
    let mut nodes = 0_u32;
    let mut roots = Vec::new();
    clip_recurse(
        a,
        b,
        (0.0, 1.0),
        (0.0, 1.0),
        0,
        &mut nodes,
        &mut roots,
        options,
        stats,
    )?;
    roots.sort_by(|x, y| x.0.total_cmp(&y.0).then_with(|| x.1.total_cmp(&y.1)));
    roots.dedup_by(|later, earlier| {
        if (later.0 - earlier.0).abs() <= ROOT_MERGE_EPS
            && (later.1 - earlier.1).abs() <= ROOT_MERGE_EPS
        {
            earlier.0 = (earlier.0 + later.0) * 0.5;
            earlier.1 = (earlier.1 + later.1) * 0.5;
            true
        } else {
            false
        }
    });
    Some(roots)
}

#[allow(clippy::too_many_arguments)]
fn clip_recurse(
    a: &Quad,
    b: &Quad,
    interval_a: (f64, f64),
    interval_b: (f64, f64),
    depth: u32,
    nodes: &mut u32,
    roots: &mut Vec<(f64, f64)>,
    options: BooleanOptions,
    stats: &mut BooleanStats,
) -> Option<()> {
    *nodes += 1;
    if *nodes > MAX_CLIP_NODES {
        return None;
    }
    bump(&mut stats.pair_tests, options.limits.max_pair_tests)?;
    let mut ta = interval_a;
    let mut tb = interval_b;
    let mut prev_width_a = ta.1 - ta.0;
    let mut prev_width_b = tb.1 - tb.0;
    let mut stalls = 0_u32;
    for _round in 0..MAX_CLIP_ROUNDS {
        let b_sub = b.subcurve(tb.0, tb.1);
        let (base, normal, dmin, dmax) = b_sub.fat_line();
        let clip_a = clip_to_band(&a.subcurve(ta.0, ta.1), base, normal, dmin, dmax)?;
        ta = (
            ta.0 + clip_a.0 * (ta.1 - ta.0),
            ta.0 + clip_a.1 * (ta.1 - ta.0),
        );
        let a_sub = a.subcurve(ta.0, ta.1);
        let (base, normal, dmin, dmax) = a_sub.fat_line();
        let clip_b = clip_to_band(&b.subcurve(tb.0, tb.1), base, normal, dmin, dmax)?;
        tb = (
            tb.0 + clip_b.0 * (tb.1 - tb.0),
            tb.0 + clip_b.1 * (tb.1 - tb.0),
        );
        let width_a = ta.1 - ta.0;
        let width_b = tb.1 - tb.0;
        if std::env::var_os("FMN_CLIP_TRACE").is_some() {
            eprintln!("    depth={depth} widths=({width_a:.3e},{width_b:.3e}) ta={ta:?} tb={tb:?}");
        }
        if width_a <= CLIP_T_EPS && width_b <= CLIP_T_EPS {
            roots.push(((ta.0 + ta.1) * 0.5, (tb.0 + tb.1) * 0.5));
            return Some(());
        }
        let shrank = width_a < 0.8 * prev_width_a && width_b < 0.8 * prev_width_b;
        if shrank {
            stalls = 0;
        } else {
            stalls += 1;
        }
        prev_width_a = width_a;
        prev_width_b = width_b;
        if stalls >= 2 {
            if depth >= MAX_CLIP_DEPTH {
                return None;
            }
            if width_a >= width_b {
                let mid = (ta.0 + ta.1) * 0.5;
                if mid <= ta.0 || mid >= ta.1 {
                    return None;
                }
                clip_recurse(
                    a,
                    b,
                    (ta.0, mid),
                    tb,
                    depth + 1,
                    nodes,
                    roots,
                    options,
                    stats,
                )?;
                return clip_recurse(
                    a,
                    b,
                    (mid, ta.1),
                    tb,
                    depth + 1,
                    nodes,
                    roots,
                    options,
                    stats,
                );
            }
            let mid = (tb.0 + tb.1) * 0.5;
            if mid <= tb.0 || mid >= tb.1 {
                return None;
            }
            clip_recurse(
                a,
                b,
                ta,
                (tb.0, mid),
                depth + 1,
                nodes,
                roots,
                options,
                stats,
            )?;
            return clip_recurse(
                a,
                b,
                ta,
                (mid, tb.1),
                depth + 1,
                nodes,
                roots,
                options,
                stats,
            );
        }
    }
    None
}

/// The convex hull of `{ t ∈ [0,1] : dmin ≤ E(t) ≤ dmax }` for the
/// distance polynomial with Bernstein coefficients `(e0, e1, e2)`.
/// `None` means the band is missed entirely (fat-line pruning).
fn clip_to_band(q: &Quad, base: Point, normal: Point, dmin: f64, dmax: f64) -> Option<(f64, f64)> {
    let e0 = dot(normal, sub(q.p0, base));
    let e1 = dot(normal, sub(q.p1, base));
    let e2 = dot(normal, sub(q.p2, base));
    let mut cuts = vec![0.0, 1.0];
    for bound in [dmin, dmax] {
        let a = e0 - 2.0 * e1 + e2;
        let b = 2.0 * (e1 - e0);
        let c = e0 - bound;
        if a == 0.0 {
            if b != 0.0 {
                cuts.push(-c / b);
            }
        } else {
            let discriminant = b * b - 4.0 * a * c;
            if discriminant >= 0.0 {
                let root = fmn_dmath::sqrt(discriminant);
                cuts.push((-b - root) / (2.0 * a));
                cuts.push((-b + root) / (2.0 * a));
            }
        }
    }
    cuts.sort_by(f64::total_cmp);
    let mut interval: Option<(f64, f64)> = None;
    for pair in cuts.windows(2) {
        let mid = (pair[0] + pair[1]) * 0.5;
        if !(0.0..=1.0).contains(&mid) {
            continue;
        }
        let mt = 1.0 - mid;
        let e = e0 * mt * mt + 2.0 * e1 * mt * mid + e2 * mid * mid;
        if e >= dmin && e <= dmax {
            let lo = pair[0].max(0.0);
            let hi = pair[1].min(1.0);
            interval = Some(match interval {
                Some((l, h)) => (l.min(lo), h.max(hi)),
                None => (lo, hi),
            });
        }
    }
    interval
}

/// Bounded Newton polish on `F(t, u) = A(t) − B(u)` against the
/// original pieces (spec §3.3). `None` refuses singular, divergent, or
/// non-converging roots.
fn polish(a: &Quad, b: &Quad, t_seed: f64, u_seed: f64, scene_scale: f64) -> Option<(f64, f64)> {
    let tolerance = 4.0 * f64::EPSILON * scene_scale;
    let mut t = t_seed.clamp(0.0, 1.0);
    let mut u = u_seed.clamp(0.0, 1.0);
    for _iteration in 0..MAX_NEWTON_ITERATIONS {
        let f = sub(a.eval(t), b.eval(u));
        if length(f) <= tolerance {
            return Some((t, u));
        }
        let da = a.deriv(t);
        let db = b.deriv(u);
        let det = cross_dd(da, db).value();
        if det.abs() <= JACOBIAN_SINGULAR * length(da) * length(db) {
            return None;
        }
        let dt = -cross_dd(f, db).value() / det;
        let du = -cross_dd(f, da).value() / det;
        t += dt;
        u += du;
        if !(0.0..=1.0).contains(&t) || !(0.0..=1.0).contains(&u) {
            return None;
        }
        if dt == 0.0 && du == 0.0 {
            return (length(sub(a.eval(t), b.eval(u))) <= tolerance).then_some((t, u));
        }
    }
    (length(sub(a.eval(t), b.eval(u))) <= tolerance).then_some((t, u))
}

/// One curve edge of the arrangement: a split piece between two vertices.
#[derive(Clone, Copy)]
struct AtomicEdge {
    operand: usize,
    quad: Quad,
    from: usize,
    to: usize,
}

/// Split every piece at its intersection parameters; split anchors snap
/// to the shared vertex (spec §3.6).
fn split_into_atomic_edges(
    pieces: &[Piece],
    intersections: &[Intersection],
    vertices: &[Point],
) -> Option<Vec<AtomicEdge>> {
    let mut edges = Vec::new();
    for (index, piece) in pieces.iter().enumerate() {
        let mut marks: Vec<(f64, usize)> = intersections
            .iter()
            .filter_map(|hit| {
                if hit.piece_a == index {
                    Some((hit.t_a, hit.vertex))
                } else if hit.piece_b == index {
                    Some((hit.t_b, hit.vertex))
                } else {
                    None
                }
            })
            .collect();
        marks.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut t_prev = 0.0;
        let mut from = piece.start_vertex;
        for &(t, vertex) in &marks {
            push_atomic_edge(piece, t_prev, t, from, vertex, vertices, &mut edges)?;
            t_prev = t;
            from = vertex;
        }
        push_atomic_edge(
            piece,
            t_prev,
            1.0,
            from,
            piece.end_vertex,
            vertices,
            &mut edges,
        )?;
    }
    Some(edges)
}

fn push_atomic_edge(
    piece: &Piece,
    t0: f64,
    t1: f64,
    from: usize,
    to: usize,
    vertices: &[Point],
    edges: &mut Vec<AtomicEdge>,
) -> Option<()> {
    let mut quad = piece.quad.subcurve(t0, t1);
    quad.p0 = vertices[from];
    quad.p2 = vertices[to];
    if quad.p0 == quad.p1 || quad.p1 == quad.p2 || quad.p0 == quad.p2 {
        // Resolution collapse of a split interval (spec §3.6 refuses
        // what cannot be represented).
        return None;
    }
    edges.push(AtomicEdge {
        operand: piece.operand,
        quad,
        from,
        to,
    });
    Some(())
}

/// The boundary half-edge graph over curve edges.
struct CurveGraph<'a> {
    edges: &'a [AtomicEdge],
    boundary: Vec<bool>,
    outgoing: Vec<Vec<usize>>,
}

/// Winding classification (spec §4): left-face samples with hull
/// clearance, ray casting over atomic edges, boundary selection by the
/// operation's truth value on both sides.
fn classify<'a>(
    vertices: &[Point],
    edges: &'a [AtomicEdge],
    operation: BooleanOperation,
    options: BooleanOptions,
    stats: &mut BooleanStats,
) -> Option<CurveGraph<'a>> {
    let mut boundary = vec![false; edges.len() * 2];
    let mut outgoing = vec![Vec::new(); vertices.len()];
    for (index, edge) in edges.iter().enumerate() {
        let direction = edge.quad.deriv(0.5);
        let direction_length = length(direction);
        let edge_length = length(sub(edge.quad.p2, edge.quad.p0));
        if edge_length == 0.0 || direction_length == 0.0 {
            return None;
        }
        let mid = edge.quad.eval(0.5);
        let mut clearance = edge_length;
        for (other_index, other) in edges.iter().enumerate() {
            if index == other_index {
                continue;
            }
            bump(
                &mut stats.classification_tests,
                options.limits.max_classification_tests,
            )?;
            clearance = clearance.min(point_triangle_distance(mid, &other.quad));
        }
        let base_offset = options.tolerance.min(edge_length).min(clearance) * 0.25;
        if !base_offset.is_finite() || base_offset <= coordinate_resolution(mid) {
            return None;
        }
        let normal = [
            -direction[1] / direction_length,
            direction[0] / direction_length,
        ];
        let mut left_winding = None;
        let mut offset = base_offset;
        for _attempt in 0..SAMPLE_ATTEMPTS {
            let sample = add(mid, scale(normal, offset));
            if sample == mid || cross_dd(direction, sub(sample, mid)).sign() <= 0 {
                return None;
            }
            if let Some(winding) = winding_at(sample, edges, options, stats)? {
                left_winding = Some(winding);
                break;
            }
            offset *= 0.5;
        }
        let (left_subject, left_clip) = left_winding?;
        let (right_subject, right_clip) = if edge.operand == 0 {
            (left_subject - 1, left_clip)
        } else {
            (left_subject, left_clip - 1)
        };
        let left_filled = operation_value(
            operation,
            options.subject_fill_rule.contains(left_subject),
            options.clip_fill_rule.contains(left_clip),
        );
        let right_filled = operation_value(
            operation,
            options.subject_fill_rule.contains(right_subject),
            options.clip_fill_rule.contains(right_clip),
        );
        boundary[index * 2] = left_filled && !right_filled;
        boundary[index * 2 + 1] = right_filled && !left_filled;
        outgoing[edge.from].push(index * 2);
        outgoing[edge.to].push(index * 2 + 1);
    }
    for (vertex, list) in outgoing.iter_mut().enumerate() {
        list.sort_by(|&a, &b| {
            angle_cmp(
                departure_tangent(edges, a, vertex),
                departure_tangent(edges, b, vertex),
            )
            .then_with(|| a.cmp(&b))
        });
    }
    Some(CurveGraph {
        edges,
        boundary,
        outgoing,
    })
}

/// The direction in which a half-edge leaves `vertex`: the curve
/// tangent at the shared endpoint.
fn departure_tangent(edges: &[AtomicEdge], half_edge: usize, vertex: usize) -> Point {
    let edge = edges[half_edge / 2];
    if half_edge.is_multiple_of(2) {
        debug_assert!(edge.from == vertex);
        sub(edge.quad.p1, edge.quad.p0)
    } else {
        debug_assert!(edge.to == vertex);
        sub(edge.quad.p1, edge.quad.p2)
    }
}

/// Winding counts at a ray-cast sample (spec §4). `Ok(None)` means the
/// ray is ambiguous (endpoint parameter, on-edge crossing, or zero
/// derivative at a root) and the caller retries with a new offset;
/// outer `None` declines the class (budget).
fn winding_at(
    sample: Point,
    edges: &[AtomicEdge],
    options: BooleanOptions,
    stats: &mut BooleanStats,
) -> Option<Option<(i64, i64)>> {
    let mut subject = 0_i64;
    let mut clip = 0_i64;
    let resolution = coordinate_resolution(sample);
    for edge in edges {
        bump(
            &mut stats.classification_tests,
            options.limits.max_classification_tests,
        )?;
        let y0 = edge.quad.p0[1] - sample[1];
        let y1 = edge.quad.p1[1] - sample[1];
        let y2 = edge.quad.p2[1] - sample[1];
        let a = y0 - 2.0 * y1 + y2;
        let b = 2.0 * (y1 - y0);
        let mut roots = Vec::new();
        if a == 0.0 {
            if b != 0.0 {
                roots.push(-y0 / b);
            }
        } else {
            let discriminant = b * b - 4.0 * a * y0;
            if discriminant > 0.0 {
                let root = fmn_dmath::sqrt(discriminant);
                roots.push((-b - root) / (2.0 * a));
                roots.push((-b + root) / (2.0 * a));
            } else if discriminant == 0.0 {
                roots.push(-b / (2.0 * a));
            }
        }
        for t in roots {
            if !(0.0..1.0).contains(&t) {
                continue;
            }
            if t < RAY_PARAM_SNAP || t > 1.0 - RAY_PARAM_SNAP {
                return Some(None);
            }
            let dy = edge.quad.deriv(t)[1];
            if dy == 0.0 {
                return Some(None);
            }
            let x = edge.quad.eval(t)[0];
            if (x - sample[0]).abs() <= resolution {
                return Some(None);
            }
            if x > sample[0] {
                let crossing = if dy > 0.0 { 1 } else { -1 };
                if edge.operand == 0 {
                    subject += crossing;
                } else {
                    clip += crossing;
                }
            }
        }
    }
    Some(Some((subject, clip)))
}

/// Lower bound on the distance from `point` to a curve: the distance to
/// its control triangle (spec §4).
fn point_triangle_distance(point: Point, q: &Quad) -> f64 {
    let o01 = cross_dd(sub(q.p1, q.p0), sub(point, q.p0)).sign();
    let o12 = cross_dd(sub(q.p2, q.p1), sub(point, q.p1)).sign();
    let o20 = cross_dd(sub(q.p0, q.p2), sub(point, q.p2)).sign();
    if (o01 >= 0 && o12 >= 0 && o20 >= 0) || (o01 <= 0 && o12 <= 0 && o20 <= 0) {
        return 0.0;
    }
    point_segment_distance(point, q.p0, q.p1)
        .min(point_segment_distance(point, q.p1, q.p2))
        .min(point_segment_distance(point, q.p2, q.p0))
}

/// A traced result loop: oriented curve pieces, end to start.
struct Loop {
    quads: Vec<Quad>,
    area: f64,
}

impl CurveGraph<'_> {
    /// Face traversal: the same next-before-the-twin rule as Stage 1
    /// (spec §5), over tangent-sorted outgoing half-edges.
    fn trace_loops(&self, options: BooleanOptions, stats: &mut BooleanStats) -> Option<Vec<Loop>> {
        let mut visited = vec![false; self.boundary.len()];
        let mut loops = Vec::new();
        let mut traced = 0_usize;
        for start in 0..self.boundary.len() {
            if !self.boundary[start] || visited[start] {
                continue;
            }
            let mut current = start;
            let mut quads = Vec::new();
            loop {
                if visited[current] {
                    if current != start {
                        return None;
                    }
                    break;
                }
                if traced == options.limits.max_output_segments {
                    return None;
                }
                traced += 1;
                visited[current] = true;
                let edge = self.edges[current / 2];
                quads.push(if current % 2 == 0 {
                    edge.quad
                } else {
                    Quad {
                        p0: edge.quad.p2,
                        p1: edge.quad.p1,
                        p2: edge.quad.p0,
                    }
                });
                current = self.next_boundary(current)?;
            }
            if quads.len() < 2 {
                return None;
            }
            let area = quads
                .iter()
                .fold(Dd::ZERO, |acc, q| acc.add(q.area_times_six()));
            if area.sign() == 0 {
                return None;
            }
            loops.push(Loop {
                quads,
                area: area.value() / 6.0,
            });
        }
        stats.output_segments = loops.iter().map(|l| l.quads.len()).sum();
        loops.sort_by(|a, b| {
            point_cmp(a.quads[0].p0, b.quads[0].p0)
                .then_with(|| a.area.total_cmp(&b.area))
                .then_with(|| a.quads.len().cmp(&b.quads.len()))
        });
        Some(loops)
    }

    fn next_boundary(&self, current: usize) -> Option<usize> {
        let edge = self.edges[current / 2];
        let destination = if current.is_multiple_of(2) {
            edge.to
        } else {
            edge.from
        };
        let twin = current ^ 1;
        let outgoing = &self.outgoing[destination];
        let twin_position = outgoing.iter().position(|&edge| edge == twin)?;
        for step in 1..=outgoing.len() {
            let candidate = outgoing[(twin_position + outgoing.len() - step) % outgoing.len()];
            if self.boundary[candidate] {
                return Some(candidate);
            }
        }
        None
    }
}

/// Emit loops as closed quadratic subpaths (spec §5).
fn emit_path(loops: &[Loop]) -> Option<QuadPath> {
    let mut path = QuadPath::new();
    for contour in loops {
        let mut points: Vec<super::Vec3> = Vec::with_capacity(contour.quads.len() * 2 + 1);
        for quad in &contour.quads {
            points.push(to_vec3(quad.p0));
            points.push(to_vec3(quad.p1));
        }
        let first = points[0];
        points.push(first);
        path.add_subpath(&points).ok()?;
    }
    Some(path)
}

/// Budget bump: exceeding a declared limit declines the class (the
/// fallback then surfaces its own deterministic typed error).
fn bump(counter: &mut usize, limit: usize) -> Option<()> {
    if *counter == limit {
        return None;
    }
    *counter += 1;
    Some(())
}
