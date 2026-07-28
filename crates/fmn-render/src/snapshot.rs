//! Canonical IR snapshots: the bit-lockable form of a compiled render plan
//! (§10.8's acceptance, §16.5's self-golden discipline).
//!
//! §16.5 makes FrankenManim's *own* bit-locked output the regression gate that
//! actually blocks merges, and the render IR is the first artifact in the
//! pipeline worth locking: it sits between the object model and the pixels, so a
//! drift here is a drift in every frame downstream, and it is small enough to
//! diff by eye when a golden moves.
//!
//! ## What is in the snapshot, and what is deliberately not
//!
//! **In:** everything that decides pixels — segment control points and their
//! arc-length spans, hull bounds, the primitive hint, every interned style
//! field, and the painter-ordered instance list with its shape, style, offset
//! and order.
//!
//! **Out: handles.** A `Mob` is `(index, generation)` into a slot arena, so it
//! encodes *allocation history* rather than content — insert an unrelated
//! mobject earlier in a fixture and every handle downstream shifts. A golden
//! that moved for that reason would be adjudicated once and then re-blessed
//! forever, which is the failure mode §16.5's "self-goldens are adjudicated, not
//! re-blessed" rule exists to prevent. Instance *order* is in the snapshot, and
//! order is what painter semantics actually promise.
//!
//! **Out: the content digest of each shape.** It is a function of the points,
//! which are already in the snapshot, so including it would lock the hash
//! function rather than the geometry — and `shape_digest`'s job is to make
//! interning work, not to be a golden.
//!
//! ## Canonical, not merely deterministic
//!
//! Serialization rides `fmn-hash`'s versioned envelope, which canonicalizes
//! floats at the boundary (`-0.0 → +0.0`, one canonical NaN) and pins field
//! order, magic and version. Two platforms that compute the same IR therefore
//! *hash* the same, which is what makes an IR golden usable across the certified
//! matrix rather than only on the machine that blessed it.

use crate::hint::Hint;
use crate::plan::RenderPlan;
use fmn_hash::{Digest, Schema, Writer};

/// The IR snapshot's schema: magic, id, and a version pair.
///
/// Bumping the minor version is how a *compatible* IR extension announces
/// itself; the major version is for a reshape. Either way the digest moves, and
/// the golden it moves is adjudicated rather than re-blessed.
pub const SNAPSHOT_SCHEMA: Schema = Schema::new(*b"FMNR", 2, 1, 1);

/// Serialize a plan canonically.
///
/// # Errors
/// Propagates `fmn_hash`'s envelope errors — a snapshot larger than the writer's
/// declared limits is the only realistic one, and it is a real failure rather
/// than something to swallow.
pub fn encode(plan: &RenderPlan) -> Result<Vec<u8>, fmn_hash::SerialError> {
    let mut w = Writer::new(SNAPSHOT_SCHEMA);

    let segments = plan.segments();
    w.put_u64(segments.len() as u64);
    for s in segments {
        for p in [s.p0, s.p1, s.p2] {
            for c in p {
                w.put_f64(c);
            }
        }
        w.put_f64(s.s0);
        w.put_f64(s.s1);
    }

    let shapes = plan.shapes().shapes();
    w.put_u64(shapes.len() as u64);
    for shape in shapes {
        w.put_u32(shape.first_segment);
        w.put_u32(shape.segment_count);
        w.put_str(shape.hint.name());
        for v in [shape.bounds.min, shape.bounds.mid, shape.bounds.max] {
            for c in v {
                w.put_f64(c);
            }
        }
        w.put_f64(shape.arc_length.total());
    }

    let styles = plan.styles().rows();
    w.put_u64(styles.len() as u64);
    for st in styles {
        for c in st
            .stroke_rgba
            .iter()
            .chain(st.stroke_rgba_end.iter())
            .chain(st.fill_rgba.iter())
            .chain(st.fill_rgba_end.iter())
        {
            w.put_f64(f64::from(*c));
        }
        for v in [
            st.stroke_width,
            st.stroke_width_end,
            st.fill_border_width,
            st.anti_alias_width,
        ] {
            w.put_f64(f64::from(v));
        }
        w.put_f64(st.joint_type.to_code());
        w.put_f64(st.is_fixed_in_frame);
        for component in st.shading {
            w.put_f64(component);
        }
        for component in st.clip_planes.iter().flatten() {
            w.put_f64(*component);
        }
        w.put_bool(st.flat_stroke);
        w.put_bool(st.scale_stroke_with_zoom);
        w.put_bool(st.stroke_behind);
        w.put_bool(st.depth_test);
    }

    let instances = plan.shapes().instances();
    w.put_u64(instances.len() as u64);
    for i in instances {
        w.put_u32(i.shape);
        w.put_u32(i.style);
        w.put_u32(i.order);
        for c in i.offset {
            w.put_f64(c);
        }
    }

    w.finish()
}

/// The plan's content digest — the value a golden pins.
///
/// # Errors
/// See [`encode`].
pub fn digest(plan: &RenderPlan) -> Result<Digest, fmn_hash::SerialError> {
    encode(plan).map(|bytes| fmn_hash::sha256(&bytes))
}

/// A human-readable dump, for the diff a reviewer reads when a golden moves.
///
/// A digest says *that* the IR changed; this says *what* changed, and without it
/// an adjudication is a guess. Deliberately lossy where the digest is not —
/// coordinates print at a fixed precision — because it is read by people.
#[must_use]
pub fn describe(plan: &RenderPlan) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "segments {} shapes {} styles {} instances {} (instances/shape {:.2})",
        plan.segments().len(),
        plan.shapes().shapes().len(),
        plan.styles().len(),
        plan.shapes().instances().len(),
        plan.shapes().instances_per_shape(),
    );
    for (i, shape) in plan.shapes().shapes().iter().enumerate() {
        let _ = writeln!(
            out,
            "  shape {i}: hint={} segments {}..{} len {:.6} bounds [{:.4},{:.4}]..[{:.4},{:.4}]",
            shape.hint.name(),
            shape.first_segment,
            shape.first_segment + shape.segment_count,
            shape.arc_length.total(),
            shape.bounds.min[0],
            shape.bounds.min[1],
            shape.bounds.max[0],
            shape.bounds.max[1],
        );
    }
    for (i, st) in plan.styles().rows().iter().enumerate() {
        let _ = writeln!(
            out,
            "  style {i}: stroke {:?}->{:?} w {:.4}->{:.4} fill {:?}->{:?} border {:.4} aa {:.4} \
             joint {:?} fixed {:.4} shading {:?} clip {:?} flat {} zoom {} behind {} depth {}",
            st.stroke_rgba,
            st.stroke_rgba_end,
            st.stroke_width,
            st.stroke_width_end,
            st.fill_rgba,
            st.fill_rgba_end,
            st.fill_border_width,
            st.anti_alias_width,
            st.joint_type,
            st.is_fixed_in_frame,
            st.shading,
            st.clip_planes,
            st.flat_stroke,
            st.scale_stroke_with_zoom,
            st.stroke_behind,
            st.depth_test,
        );
    }
    for inst in plan.shapes().instances() {
        let _ = writeln!(
            out,
            "  draw {}: shape {} style {} at [{:.4},{:.4},{:.4}]",
            inst.order, inst.shape, inst.style, inst.offset[0], inst.offset[1], inst.offset[2],
        );
    }
    out
}

/// True when a hint is one the snapshot's `hint.name()` can round-trip.
///
/// Every variant can, and this exists so that adding a `Hint` variant without a
/// name fails a test here rather than silently collapsing two hints into one
/// snapshot value.
#[must_use]
pub fn hint_names_are_distinct(hints: &[Hint]) -> bool {
    let mut names: Vec<&str> = hints.iter().map(|h| h.name()).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    names.len() == before
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::RenderPlan;
    use fmn_mobject::{Mobject, RecordBuffer, RecordSchema, ShapeTag, Stage, Uniforms};

    /// The fixture scene the golden below pins.
    ///
    /// Built to exercise what the snapshot claims to cover: two distinct
    /// outlines, one of them repeated (so instancing shows), two distinct styles
    /// (so interning shows), and a live primitive hint (so the routing shows).
    fn fixture() -> Stage {
        let mut stage = Stage::new();

        let make = |dx: f64, width: f32, rgba: [f32; 4]| -> Mobject {
            let pts: Vec<[f64; 3]> = vec![
                [dx, 0.0, 0.0],
                [dx + 1.0, 0.0, 0.0],
                [dx + 2.0, 0.0, 0.0],
                [dx + 2.0, 1.0, 0.0],
                [dx + 2.0, 2.0, 0.0],
            ];
            let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), pts.len());
            for (i, p) in pts.iter().enumerate() {
                buffer.write(i, "point", &[p[0] as f32, p[1] as f32, p[2] as f32]);
                buffer.write(i, "stroke_rgba", &rgba);
                buffer.write(i, "stroke_width", &[width]);
            }
            Mobject::from_buffer(buffer)
        };

        // Two copies of one outline, one style.
        for dx in [0.0, 10.0] {
            let mob = stage.add(make(dx, 2.0, [1.0, 1.0, 1.0, 1.0]));
            stage.add_to_scene(mob).expect("live");
        }
        // A third, differently styled — same outline, so interning must keep one
        // shape and two style rows.
        let mob = stage.add(make(20.0, 4.0, [1.0, 0.0, 0.0, 1.0]));
        stage.add_to_scene(mob).expect("live");

        // A genuinely distinct outline, with a hint that is *true* of its
        // points. Truthfulness matters more here than it looks: a hint selects a
        // kernel, so tagging a polyline as a circle would be a wrong picture,
        // and Marionette's `set_shape` records what it is told.
        let arc = fmn_geom::quadpath::QuadPath::arc(
            0.0,
            std::f64::consts::TAU,
            1.0,
            [30.0, 0.0, 0.0],
            None,
        );
        let pts = arc.points();
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), pts.len());
        for (i, p) in pts.iter().enumerate() {
            buffer.write(i, "point", &[p[0] as f32, p[1] as f32, p[2] as f32]);
            buffer.write(i, "stroke_rgba", &[0.0, 0.0, 1.0, 1.0]);
            buffer.write(i, "stroke_width", &[2.0]);
        }
        let circle = stage.add(Mobject::from_buffer(buffer));
        stage.set_shape(
            circle,
            ShapeTag::Circle {
                center: [30.0, 0.0, 0.0],
                radius: 1.0,
            },
        );
        stage.add_to_scene(circle).expect("live");

        stage
    }

    fn synced() -> RenderPlan {
        let stage = fixture();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);
        plan
    }

    /// **The IR golden.**
    ///
    /// Bit-locked, per §16.5. If this moves, something in the compiled IR
    /// changed — which may be entirely correct, in which case the change is
    /// *adjudicated* (read [`describe`]'s output, confirm it is the intended
    /// difference, and update this constant in the same commit as the change
    /// that caused it). What must never happen is re-blessing it because it
    /// failed.
    ///
    /// **Moved once, 2026-07-25 (fm-tg6).** Compiled outlines became
    /// shape-local: the circle's bounds went from `[29,-1]..[31,1]` to
    /// `[-2,-1]..[0,1]`, and its instance offset `[31,0,0]` reconstructs the
    /// original exactly. That was a *fix* — `shape_digest` had always excluded
    /// position, so storing absolute points meant every copy of an interned
    /// outline rendered wherever the first copy happened to be. The golden is
    /// what surfaced it as a decision rather than a silent difference.
    ///
    /// **Moved again, 2026-07-28 (fm-diu).** Snapshot schema 1.1 adds the
    /// camera, clip, shading, stroke-construction and depth fields that were
    /// already part of Marionette's batch key but had been dropped from
    /// Lumen's retained style row. The dump above adjudicates the fixture's
    /// default values; dedicated sensitivity tests move the digest when one
    /// changes.
    const FIXTURE_DIGEST: &str = "bbf8e9c59a7a6ea6e7cbbc904b4bccbe98c0cbdc719e82b75d18f6524ab53e66";

    #[test]
    fn the_fixture_ir_is_bit_locked() {
        let plan = synced();
        let got = digest(&plan).expect("the fixture is well within the envelope's limits");
        // The failure message carries everything an adjudication needs — the new
        // digest to paste once the change is confirmed intended, and the dump
        // that says what moved. There is deliberately no "bless" mode: a switch
        // that rewrites the golden is a switch someone reaches for at 2am.
        assert_eq!(
            got.to_hex(),
            FIXTURE_DIGEST,
            "the compiled IR moved. Adjudicate, do not re-bless:\n{}",
            describe(&plan)
        );
    }

    #[test]
    fn the_snapshot_is_stable_across_syncs() {
        // A second sync reuses everything, and reuse must produce the identical
        // IR — otherwise "retained" would be a source of drift rather than a
        // saving.
        let stage = fixture();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);
        let first = digest(&plan).expect("encodes");
        let stats = plan.sync(&stage, 0);
        assert_eq!(stats.shapes_compiled, 0, "the second sync must reuse");
        assert_eq!(digest(&plan).expect("encodes"), first);
    }

    #[test]
    fn the_snapshot_moves_when_the_geometry_moves() {
        // The complement, and the reason a golden is worth anything: it has to
        // be sensitive to the thing it locks.
        let stage = fixture();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);
        let before = digest(&plan).expect("encodes");

        let mut stage2 = fixture();
        let mob = stage2.draw_plan().items()[0].mob;
        stage2
            .get_mut(mob)
            .expect("live")
            .buffer
            .write(2, "point", &[7.0, 7.0, 0.0]);
        let mut plan2 = RenderPlan::new();
        plan2.sync(&stage2, 0);
        assert_ne!(digest(&plan2).expect("encodes"), before);
    }

    #[test]
    fn the_snapshot_moves_when_only_a_style_moves() {
        let stage = fixture();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);
        let before = digest(&plan).expect("encodes");

        let mut stage2 = fixture();
        let mob = stage2.draw_plan().items()[0].mob;
        stage2
            .get_mut(mob)
            .expect("live")
            .buffer
            .write(0, "stroke_rgba", &[0.0, 1.0, 0.0, 1.0]);
        let mut plan2 = RenderPlan::new();
        plan2.sync(&stage2, 0);
        assert_ne!(digest(&plan2).expect("encodes"), before);
    }

    #[test]
    fn the_snapshot_moves_when_each_render_uniform_moves() {
        let stage = fixture();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);
        let before = digest(&plan).expect("encodes");

        let cases = [
            (
                "fixed",
                Uniforms {
                    is_fixed_in_frame: 0.5,
                    ..Uniforms::default()
                },
            ),
            (
                "shading",
                Uniforms {
                    shading: [0.1, 0.2, 0.3],
                    ..Uniforms::default()
                },
            ),
            (
                "clip",
                Uniforms {
                    clip_planes: [[1.0, 0.0, 0.0, -1.0], [0.0; 4], [0.0; 4], [0.0; 4]],
                    ..Uniforms::default()
                },
            ),
            (
                "flat",
                Uniforms {
                    flat_stroke: true,
                    ..Uniforms::default()
                },
            ),
            (
                "zoom",
                Uniforms {
                    scale_stroke_with_zoom: true,
                    ..Uniforms::default()
                },
            ),
            (
                "behind",
                Uniforms {
                    stroke_behind: true,
                    ..Uniforms::default()
                },
            ),
            (
                "depth",
                Uniforms {
                    depth_test: true,
                    ..Uniforms::default()
                },
            ),
        ];
        for (name, uniforms) in cases {
            let mut changed = fixture();
            let mob = changed.draw_plan().items()[0].mob;
            *changed.uniforms_mut(mob).expect("live") = uniforms;
            let mut changed_plan = RenderPlan::new();
            changed_plan.sync(&changed, 0);
            assert_ne!(
                digest(&changed_plan).expect("encodes"),
                before,
                "{name} must participate in the snapshot"
            );
        }
    }

    #[test]
    fn the_snapshot_moves_when_only_the_order_moves() {
        // Painter order is semantics (§8.5), so it is in the digest even though
        // no geometry and no style changed.
        let stage = fixture();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);
        let before = digest(&plan).expect("encodes");

        let mut stage2 = fixture();
        let mob = stage2.draw_plan().items()[0].mob;
        stage2.set_z_index(mob, 5, false);
        stage2.add_to_scene(mob).expect("live");
        let mut plan2 = RenderPlan::new();
        plan2.sync(&stage2, 0);
        assert_ne!(digest(&plan2).expect("encodes"), before);
    }

    #[test]
    fn the_first_outline_to_compile_decides_the_shared_hint() {
        // Interning keys on geometry, and a hint describes geometry — so two
        // mobjects with identical points share one hint, and it is the first
        // compiler's. That is sound (a hint true of those points is true of the
        // same points) and it is a missed *optimization* when the later one
        // carried a better tag: an untagged copy of a circle keeps `general`.
        // Recorded because the alternative — re-deriving the hint per instance —
        // would put a per-occurrence branch in the thing interning exists to
        // make cheap.
        let plan = synced();
        let shapes = plan.shapes().shapes();
        assert_eq!(shapes.len(), 2);
        assert!(
            shapes[0].hint.is_general(),
            "the shared polyline is untagged"
        );
        assert_eq!(shapes[1].hint.name(), "circle", "the arc carries its tag");
    }

    #[test]
    fn the_fixture_actually_exercises_interning() {
        // A golden over a scene with nothing to intern would pass forever while
        // the mechanism rotted.
        let plan = synced();
        assert!(
            plan.shapes().instances().len() > plan.shapes().shapes().len(),
            "the fixture must have at least one shared outline: {}",
            describe(&plan)
        );
        assert!(
            plan.styles().len() > 1,
            "and at least two distinct styles: {}",
            describe(&plan)
        );
    }

    #[test]
    fn every_hint_has_its_own_snapshot_name() {
        assert!(hint_names_are_distinct(&[
            Hint::General,
            Hint::Line,
            Hint::Polyline { closed: true },
            Hint::Arc {
                center: [0.0; 3],
                radius: 1.0,
                start_angle: 0.0,
                angle: 1.0
            },
            Hint::Circle {
                center: [0.0; 3],
                radius: 1.0
            },
            Hint::Dot {
                center: [0.0; 3],
                radius: 1.0
            },
            Hint::Rect {
                center: [0.0; 3],
                width: 1.0,
                height: 1.0
            },
            Hint::RoundedRect {
                center: [0.0; 3],
                width: 1.0,
                height: 1.0,
                corner_radius: 0.1
            },
        ]));
    }

    #[test]
    fn describe_names_every_row_it_claims_to() {
        let plan = synced();
        let text = describe(&plan);
        assert!(text.contains("segments"));
        assert!(text.contains("shape 0"));
        assert!(text.contains("style 0"));
        assert!(text.contains("draw 0"));
    }
}
