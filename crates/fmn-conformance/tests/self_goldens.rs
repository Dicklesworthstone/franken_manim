//! The first live self-goldens (§16.3 plane 2, D-16, fm-xb3): geometry
//! snapshots at lifecycle points, bit-locked per platform.
//!
//! Two artifacts are locked in `goldens/self_goldens.<platform>.lock`:
//!
//! - `geom_lifecycle.v1` — a QuadPath driven through its construction
//!   lifecycle (arc → line → smooth curve → close), with the full point run
//!   snapshotted after each step;
//! - `stage_lifecycle.v1` — a three-mobject Stage family driven through the
//!   positional API (attach → next_to → arrange → scale → to_edge), with
//!   every member's f32 records and the root bounding box snapshotted.
//!
//! Snapshots are serialized through fmn-hash's canonical Writer (versioned
//! schema, defined field order, float canonicalization, trailing checksum),
//! so the locked bytes are the §6.7 durable form, not a Debug dump. Locks are
//! per-platform ([`Scope::PerPlatform`]); each certified-matrix platform
//! contributes its own lock file the first time the suite runs there, and
//! cross-platform convergence graduates to [`Scope::Certified`] when the
//! certified arithmetic lands (G0-6).
//!
//! Drift fails here — this is the merge blocker. Deliberate changes re-bless
//! with `UPDATE_GOLDENS=1 cargo test -p fmn-conformance --test self_goldens`
//! and commit the lock diff (the rig never commits; frame hashes join these
//! artifacts once Lumen exists).

use fmn_conformance::golden::{GoldenStore, Scope};
use fmn_core::constants::{DOWN, LEFT, RIGHT, TAU, UP};
use fmn_core::types::Vec3;
use fmn_geom::QuadPath;
use fmn_hash::{Schema, Writer};
use fmn_mobject::{Mob, Mobject, Stage};
use std::path::PathBuf;

/// Schema family for self-golden snapshot documents.
const GEOM_SCHEMA: Schema = Schema::new(*b"FMNS", 1, 1, 0);
const STAGE_SCHEMA: Schema = Schema::new(*b"FMNS", 2, 1, 0);

fn store() -> GoldenStore {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens");
    GoldenStore::new(dir, "self_goldens", Scope::PerPlatform).expect("store")
}

/// Append a labeled point run to the document: label, count, then x/y/z f64s.
fn put_points_f64(w: &mut Writer, label: &str, points: &[Vec3]) {
    w.put_str(label).put_u64(points.len() as u64);
    for p in points {
        for &c in p {
            w.put_f64(c);
        }
    }
}

/// The QuadPath lifecycle document: point snapshots after each construction
/// step, under the original Reference op names (§7).
fn geom_lifecycle_doc() -> Vec<u8> {
    let mut w = Writer::new(GEOM_SCHEMA);

    // Step 1: a quarter arc of radius 1.5 centered off-origin.
    let mut path = QuadPath::arc(0.0, TAU / 4.0, 1.5, [0.5, -0.25, 0.0], Some(4));
    put_points_f64(&mut w, "arc", path.points());

    // Step 2: a line to a corner point.
    path.add_line_to([2.0, 2.0, 0.0], false).expect("line");
    put_points_f64(&mut w, "line_to", path.points());

    // Step 3: a smooth continuation (reflected-handle rule).
    path.add_smooth_curve_to([-1.0, 2.5, 0.0]).expect("smooth");
    put_points_f64(&mut w, "smooth_curve_to", path.points());

    // Step 4: close the subpath (jagged closure).
    path.close_path(false).expect("close");
    put_points_f64(&mut w, "close_path", path.points());
    w.put_bool(path.is_closed());
    w.put_u64(path.num_curves() as u64);

    w.finish().expect("geometry snapshot encodes")
}

/// Append one mobject's own f32 point records to the document.
fn put_records_f32(w: &mut Writer, label: &str, stage: &Stage, mob: Mob) {
    let col = stage
        .get(mob)
        .and_then(|e| e.buffer.read_column("point"))
        .unwrap_or_default();
    w.put_str(label).put_u64(col.len() as u64);
    for &v in &col {
        w.put_f32(v);
    }
}

/// The Stage lifecycle document: a family driven through the positional API,
/// with every member's records and the root bbox snapshotted at the end.
fn stage_lifecycle_doc() -> Vec<u8> {
    let mut w = Writer::new(STAGE_SCHEMA);
    let mut stage = Stage::new();

    // A unit square, a right triangle, and a wide bar.
    let square = stage.add(Mobject::from_points(&[
        [-0.5, -0.5, 0.0],
        [0.5, -0.5, 0.0],
        [0.5, 0.5, 0.0],
        [-0.5, 0.5, 0.0],
    ]));
    let triangle = stage.add(Mobject::from_points(&[
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
    ]));
    let bar = stage.add(Mobject::from_points(&[
        [-1.5, -0.1, 0.0],
        [1.5, -0.1, 0.0],
        [1.5, 0.1, 0.0],
        [-1.5, 0.1, 0.0],
    ]));
    let root = stage.add(Mobject::new());
    for child in [square, triangle, bar] {
        stage.attach(root, child).expect("attach");
    }

    // The positional lifecycle: relative placement, arrangement, scaling,
    // and a frame-edge alignment — each step visible in the final records.
    stage.next_to(triangle, square, RIGHT, 0.25, DOWN);
    stage.arrange(root, RIGHT, 0.5, true);
    stage.scale(root, 1.25);
    stage.to_edge(root, UP, 0.8);
    stage.next_to(bar, triangle, DOWN, 0.3, LEFT);

    for (label, mob) in [("square", square), ("triangle", triangle), ("bar", bar)] {
        put_records_f32(&mut w, label, &stage, mob);
    }

    let bb = stage.get_bounding_box(root);
    for corner in [bb.min, bb.mid, bb.max] {
        for &c in &corner {
            w.put_f64(c);
        }
    }

    w.finish().expect("stage snapshot encodes")
}

#[test]
fn geom_lifecycle_is_bit_locked() {
    let doc = geom_lifecycle_doc();
    if let Err(e) = store().check("geom_lifecycle.v1", &doc) {
        panic!("{e}");
    }
}

#[test]
fn stage_lifecycle_is_bit_locked() {
    let doc = stage_lifecycle_doc();
    if let Err(e) = store().check("stage_lifecycle.v1", &doc) {
        panic!("{e}");
    }
}

#[test]
fn snapshot_documents_are_reproducible_within_run() {
    // The rig's premise: the same engine state serializes to the same bytes.
    // A failure here is nondeterminism in the engine or the encoder, which
    // must be caught before it can masquerade as cross-commit drift.
    assert_eq!(geom_lifecycle_doc(), geom_lifecycle_doc());
    assert_eq!(stage_lifecycle_doc(), stage_lifecycle_doc());
}
