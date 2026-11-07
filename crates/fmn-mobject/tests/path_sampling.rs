//! Owned path observations retain world-space sampling through edits and teardown.

use std::sync::{Arc, Barrier};
use std::thread;

use fmn_core::types::Vec3;
use fmn_geom::{GeomError, QuadPath};
use fmn_mobject::path_sampling::{PathObservation, PathSampler};
use fmn_mobject::{Mob, Mobject, Placement, RecordBuffer, RecordSchema, Stage, StageError};

const CURVE: [[f32; 3]; 5] = [
    [-2.0, -1.0, 0.0],
    [0.0, 3.0, 2.0],
    [2.0, 0.0, 1.0],
    [4.0, -2.0, 0.0],
    [6.0, 1.0, -1.0],
];

const ALPHAS: [f64; 13] = [
    -0.25, 0.0, 0.0625, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875, 1.0, 1.5, 0.33,
];

fn records(points: &[[f32; 3]]) -> RecordBuffer {
    let mut records = RecordBuffer::new(RecordSchema::vmobject(), points.len()).unwrap();
    for (index, point) in points.iter().enumerate() {
        assert!(records.write(index, "point", point));
    }
    records
}

fn path(points: &[[f32; 3]]) -> Mobject {
    Mobject::from_buffer(records(points))
}

fn bits(point: Vec3) -> [u64; 3] {
    point.map(f64::to_bits)
}

fn assert_send_sync<T: Send + Sync>() {}

struct Capture {
    observation: PathObservation,
    expected: [[u64; 3]; ALPHAS.len()],
    label: &'static str,
}

fn capture(stage: &Stage, mob: Mob, sampler: &mut PathSampler, label: &'static str) -> Capture {
    let snapshot = stage.snapshot();
    // Every reference query starts with a new physical Stage and empty query
    // cache. Neither the sampler nor an observation supplies its answer.
    let expected = ALPHAS.map(|alpha| {
        let cold = snapshot.materialize();
        bits(cold.point_from_proportion(mob, alpha).unwrap())
    });
    let world = stage.get_points(mob).unwrap();
    let direct = QuadPath::from_points(world.clone()).unwrap();
    let observation = sampler.observe(stage, mob).unwrap();
    assert_eq!(observation.point_count(), world.len(), "{label}");
    assert_eq!(
        observation.total_length().to_bits(),
        direct.get_arc_length().to_bits(),
        "{label}",
    );
    for (index, alpha) in ALPHAS.into_iter().enumerate() {
        assert_eq!(
            bits(observation.sample(alpha).unwrap()),
            expected[index],
            "{label}",
        );
    }
    Capture {
        observation,
        expected,
        label,
    }
}

#[test]
fn observations_survive_live_views_replacement_restore_and_owner_teardown() {
    assert_send_sync::<PathObservation>();

    let mut stage = Stage::new();
    let mob = stage.add(path(&CURVE));
    let checkpoint = stage.snapshot();
    let mut sampler = PathSampler::default();
    let mut captures = Vec::new();

    let first = sampler.sample(&stage, mob, 0.375).unwrap();
    assert_eq!(sampler.materializations(), 1);
    captures.push(capture(&stage, mob, &mut sampler, "original"));
    assert_eq!(sampler.materializations(), 1);
    assert_eq!(
        bits(captures[0].observation.sample(0.375).unwrap()),
        bits(first),
    );

    let point_revision = stage.get(mob).unwrap().buffer.field_revision("point");
    stage
        .set_placement(
            mob,
            Placement::new(
                [[1.5, 0.25, 0.0], [0.0, 0.5, 0.125], [0.0, 0.0, 2.0]],
                [4.0, -2.0, 1.0],
            ),
        )
        .unwrap();
    assert_eq!(
        stage.get(mob).unwrap().buffer.field_revision("point"),
        point_revision,
    );
    captures.push(capture(&stage, mob, &mut sampler, "affine placement"));
    assert_eq!(sampler.materializations(), 2);
    assert_ne!(captures[0].expected, captures[1].expected);

    assert!(stage.bake_placement(mob).unwrap());
    captures.push(capture(&stage, mob, &mut sampler, "baked placement"));
    let view = stage
        .get_mut(mob)
        .unwrap()
        .buffer
        .export_field_view("point", true)
        .unwrap();
    let revision = stage.get(mob).unwrap().buffer.field_revision("point");
    assert!(view.write_foreign(1, "point", &[4.75, 3.5, 4.0]));
    assert_eq!(
        stage.get(mob).unwrap().buffer.field_revision("point"),
        revision,
    );
    captures.push(capture(&stage, mob, &mut sampler, "first foreign write"));
    assert_ne!(captures[2].expected, captures[3].expected);

    // While a writable view is attached, even an unchanged record revision
    // cannot authorize reusing a prior observation of its points.
    assert!(view.write_foreign(4, "point", &[8.0, -1.25, 2.0]));
    assert_eq!(
        stage.get(mob).unwrap().buffer.field_revision("point"),
        revision,
    );
    let before = sampler.materializations();
    captures.push(capture(&stage, mob, &mut sampler, "second foreign write"));
    assert_eq!(sampler.materializations(), before + 1);
    assert_ne!(captures[3].expected, captures[4].expected);

    stage
        .set_points(
            mob,
            &[
                [-1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
                [1.0, 1.0, 0.0],
                [1.0, 2.0, 0.0],
                [1.0, 3.0, 0.0],
                [2.0, 3.0, 0.0],
                [3.0, 3.0, 0.0],
            ],
        )
        .unwrap();
    assert!(!view.is_attached_to(&stage.get(mob).unwrap().buffer));
    captures.push(capture(&stage, mob, &mut sampler, "replacement topology"));
    let before = sampler.materializations();
    assert!(view.write_foreign(0, "point", &[100.0, 200.0, 300.0]));
    let detached = sampler.observe(&stage, mob).unwrap();
    assert_eq!(sampler.materializations(), before);
    for (index, alpha) in ALPHAS.into_iter().enumerate() {
        assert_eq!(
            bits(detached.sample(alpha).unwrap()),
            captures[5].expected[index],
        );
    }
    drop(detached);
    drop(view);

    stage.restore(&checkpoint);
    captures.push(capture(&stage, mob, &mut sampler, "snapshot restore"));
    assert_eq!(captures[6].expected, captures[0].expected);

    // Only immutable observations and plain reference values reach the
    // workers. No Stage, Snapshot, source record view, or sampler remains.
    stage.delete(mob).unwrap();
    sampler.clear();
    drop(sampler);
    drop(stage);
    drop(checkpoint);

    let captures = Arc::new(captures);
    for threads in [1, 4, 16] {
        let barrier = Arc::new(Barrier::new(threads));
        let mut workers = Vec::new();
        for worker in 0..threads {
            let captures = Arc::clone(&captures);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                let mut comparisons = 0_usize;
                for offset in 0..captures.len() {
                    let capture = &captures[(worker + offset) % captures.len()];
                    let observation = capture.observation.clone();
                    // Multiplication by five permutes all thirteen samples;
                    // workers start at different positions in that order.
                    for offset in 0..ALPHAS.len() {
                        let index = (worker + offset * 5) % ALPHAS.len();
                        assert_eq!(
                            bits(observation.sample(ALPHAS[index]).unwrap()),
                            capture.expected[index],
                            "{} at {threads} threads",
                            capture.label,
                        );
                        comparisons += 1;
                    }
                }
                comparisons
            }));
        }
        for worker in workers {
            assert_eq!(
                worker.join().expect("owned path replay must remain valid"),
                captures.len() * ALPHAS.len(),
            );
        }
    }
}

#[test]
fn observations_keep_the_existing_law_for_curves_breaks_and_degenerate_paths() {
    let fixtures: &[(&'static str, &[[f32; 3]])] = &[
        ("curved", &CURVE),
        ("single point", &[[3.0, -2.0, 1.0]]),
        ("zero length", &[[3.0, -2.0, 1.0]; 5]),
        (
            "leading zero-length curve",
            &[
                [0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [2.0, 0.0, 0.0],
            ],
        ),
        (
            "authored break",
            &[
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [2.0, 0.0, 0.0],
                [2.0, 0.0, 0.0],
                [10.0, 0.0, 0.0],
                [10.0, 3.0, 0.0],
                [10.0, 6.0, 0.0],
            ],
        ),
    ];
    for &(label, points) in fixtures {
        let mut stage = Stage::new();
        let mob = stage.add(path(points));
        let mut sampler = PathSampler::default();
        let captured = capture(&stage, mob, &mut sampler, label);
        drop(sampler);
        drop(stage);
        for (index, alpha) in ALPHAS.into_iter().enumerate().rev() {
            assert_eq!(
                bits(captured.observation.sample(alpha).unwrap()),
                captured.expected[index],
                "{label}",
            );
        }
        if matches!(label, "single point" | "zero length") {
            assert_eq!(captured.observation.total_length(), 0.0);
            for alpha in ALPHAS {
                assert_eq!(
                    captured.observation.sample(alpha).unwrap(),
                    [3.0, -2.0, 1.0],
                );
            }
        }
    }

    // An independent analytic witness distinguishes true arc length from
    // equal time per curve: the two straight segments have lengths two and six.
    let mut stage = Stage::new();
    let mob = stage.add(path(&[
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [2.0, 3.0, 0.0],
        [2.0, 6.0, 0.0],
    ]));
    let mut sampler = PathSampler::default();
    let captured = capture(&stage, mob, &mut sampler, "unequal straight segments");
    assert_eq!(captured.observation.total_length(), 8.0);
    for (alpha, expected) in [
        (0.125, [1.0, 0.0, 0.0]),
        (0.5, [2.0, 2.0, 0.0]),
        (0.75, [2.0, 4.0, 0.0]),
    ] {
        let actual = captured.observation.sample(alpha).unwrap();
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected).abs() <= 1e-12);
        }
    }
}

#[test]
fn failed_observations_clear_the_sampler_without_invalidating_owned_paths() {
    let mut stage = Stage::new();
    let valid = stage.add(path(&CURVE));
    let empty = stage.add(path(&[]));
    let even = stage.add(path(&[[0.0; 3], [1.0; 3]]));
    let no_points = RecordSchema::new(&[("rgba", 4)], &[], &[]).unwrap();
    let missing = stage.add(Mobject::from_buffer(
        RecordBuffer::new(no_points, 1).unwrap(),
    ));
    let stale = stage.add(path(&CURVE));
    stage.delete(stale).unwrap();
    let replacement = stage.add(path(&[[7.0, 8.0, 9.0]]));
    assert_ne!(stale, replacement);
    let mut foreign_stage = Stage::new();
    let foreign = foreign_stage.add(path(&CURVE));

    let mut sampler = PathSampler::default();
    let kept = capture(&stage, valid, &mut sampler, "kept valid path");
    for (mob, expected) in [
        (empty, StageError::SchemaMismatch),
        (
            even,
            StageError::Geometry(GeomError::EvenPointCount { len: 2 }),
        ),
        (missing, StageError::StaleHandle),
        (stale, StageError::StaleHandle),
        (foreign, StageError::StaleHandle),
    ] {
        let before = sampler.materializations();
        assert_eq!(
            stage.point_from_proportion(mob, 0.5).unwrap_err(),
            expected,
        );
        assert_eq!(
            sampler
                .observe(&stage, mob)
                .expect_err("invalid path must refuse"),
            expected,
        );
        assert_eq!(sampler.materializations(), before);
        let repaired = sampler.observe(&stage, valid).unwrap();
        assert_eq!(sampler.materializations(), before + 1);
        assert_eq!(bits(repaired.sample(0.5).unwrap()), kept.expected[6]);
        assert_eq!(bits(kept.observation.sample(0.5).unwrap()), kept.expected[6]);
    }

    let before = sampler.materializations();
    stage
        .get_mut(valid)
        .unwrap()
        .buffer
        .resize_preserving_order(2)
        .unwrap();
    assert_eq!(
        sampler
            .observe(&stage, valid)
            .expect_err("malformed live geometry must refuse"),
        StageError::Geometry(GeomError::EvenPointCount { len: 2 }),
    );
    assert_eq!(sampler.materializations(), before);
    assert_eq!(bits(kept.observation.sample(0.5).unwrap()), kept.expected[6]);
    let points = CURVE.map(|point| point.map(f64::from));
    stage.set_points(valid, &points).unwrap();
    let repaired = sampler.observe(&stage, valid).unwrap();
    assert_eq!(sampler.materializations(), before + 1);
    for (index, alpha) in ALPHAS.into_iter().enumerate() {
        assert_eq!(bits(repaired.sample(alpha).unwrap()), kept.expected[index]);
    }
}

#[test]
fn owned_observations_preserve_cache_invalidation_and_clone_rules() {
    let mut stage = Stage::new();
    let mob = stage.add(path(&CURVE));
    let storage = stage.get(mob).unwrap().buffer.storage_identity();
    let mut sampler = PathSampler::default();
    let expected = sampler.sample(&stage, mob, 0.5).unwrap();
    let observation = sampler.observe(&stage, mob).unwrap();
    assert_eq!(sampler.materializations(), 1);

    let clone = observation.clone();
    drop(observation);
    assert_eq!(bits(clone.sample(0.5).unwrap()), bits(expected));
    assert_eq!(sampler.materializations(), 1);

    let point_revision = stage.get(mob).unwrap().buffer.field_revision("point");
    assert!(
        stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .write(0, "fill_rgba", &[0.2, 0.3, 0.4, 0.5]),
    );
    assert!(
        storage.matches(&stage.get(mob).unwrap().buffer),
        "owned observations must not retain record storage or force copy-on-write",
    );
    assert_eq!(
        stage.get(mob).unwrap().buffer.field_revision("point"),
        point_revision,
    );
    let styled = sampler.observe(&stage, mob).unwrap();
    assert_eq!(sampler.materializations(), 1);
    assert_eq!(bits(styled.sample(0.5).unwrap()), bits(expected));
    assert_eq!(bits(sampler.sample(&stage, mob, 0.5).unwrap()), bits(expected));
    assert_eq!(sampler.materializations(), 1);

    let mut independent = sampler.clone();
    assert_eq!(independent.materializations(), 0);
    let observed = independent.observe(&stage, mob).unwrap();
    assert_eq!(bits(observed.sample(0.5).unwrap()), bits(expected));
    assert_eq!(independent.materializations(), 1);
    assert_eq!(sampler.materializations(), 1);

    sampler.clear();
    assert_eq!(sampler.materializations(), 1);
    let rebuilt = sampler.observe(&stage, mob).unwrap();
    assert_eq!(bits(rebuilt.sample(0.5).unwrap()), bits(expected));
    assert_eq!(sampler.materializations(), 2);
    assert_eq!(bits(clone.sample(0.5).unwrap()), bits(expected));

    // Independent storage can repeat both revisions and record count. It
    // must still invalidate the live sampler without changing old observations.
    let moved = CURVE.map(|[x, y, z]| [x + 8.0, y - 2.0, z + 0.5]);
    let mut replacement = records(&moved);
    assert!(replacement.write(0, "fill_rgba", &[0.2, 0.3, 0.4, 0.5]));
    let current = &stage.get(mob).unwrap().buffer;
    assert_eq!(replacement.len(), current.len());
    assert_eq!(replacement.revision(), current.revision());
    assert_eq!(
        replacement.field_revision("point"),
        current.field_revision("point"),
    );
    stage.get_mut(mob).unwrap().buffer = replacement;
    assert!(!storage.matches(&stage.get(mob).unwrap().buffer));
    let replacement_expected = stage
        .snapshot()
        .materialize()
        .point_from_proportion(mob, 0.5)
        .unwrap();
    assert_ne!(bits(replacement_expected), bits(expected));
    let replaced = sampler.observe(&stage, mob).unwrap();
    assert_eq!(sampler.materializations(), 3);
    assert_eq!(
        bits(replaced.sample(0.5).unwrap()),
        bits(replacement_expected),
    );
    assert_eq!(bits(clone.sample(0.5).unwrap()), bits(expected));

    // A foreign writer can edit again after its last observation. Release
    // advances the point revision, so the next query sees that final edit.
    let view = stage
        .get_mut(mob)
        .unwrap()
        .buffer
        .export_field_view("point", true)
        .unwrap();
    let in_view = capture(&stage, mob, &mut sampler, "live view before release");
    let before = sampler.materializations();
    let revision = stage.get(mob).unwrap().buffer.field_revision("point");
    assert!(view.write_foreign(2, "point", &[16.0, 7.0, 4.0]));
    assert_eq!(
        stage.get(mob).unwrap().buffer.field_revision("point"),
        revision,
    );
    drop(view);
    assert_ne!(
        stage.get(mob).unwrap().buffer.field_revision("point"),
        revision,
    );
    let released = capture(&stage, mob, &mut sampler, "released foreign view");
    assert_eq!(sampler.materializations(), before + 1);
    assert_ne!(in_view.expected, released.expected);
    for (index, alpha) in ALPHAS.into_iter().enumerate() {
        assert_eq!(
            bits(in_view.observation.sample(alpha).unwrap()),
            in_view.expected[index],
        );
    }
    assert_eq!(
        bits(sampler.sample(&stage, mob, 0.5).unwrap()),
        released.expected[6],
    );
    assert_eq!(sampler.materializations(), before + 1);
}
