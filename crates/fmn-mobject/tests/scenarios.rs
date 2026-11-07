//! The ten G0-1 lifetime scenarios as fmn-mobject's **permanent regression
//! suite** (fm-ce8), plus the arena acceptance tests: generational safety,
//! O(touched) snapshot cost, family-cache invalidation, and cycle refusal.

use fmn_mobject::{Mob, Mobject, Stage, StageError};

fn square() -> Mobject {
    Mobject::from_points(&[
        [1.0, 1.0, 0.0],
        [-1.0, 1.0, 0.0],
        [-1.0, -1.0, 0.0],
        [1.0, -1.0, 0.0],
    ])
}

// Scenario 1: detached construction.
#[test]
fn s1_detached_construction() {
    let group = Mobject::group(vec![square(), Mobject::from_points(&[[0.0, 0.0, 0.0]])]);
    let mut stage = Stage::new();
    let g = stage.add(group);
    assert_eq!(stage.family(g).len(), 3);
    let children: Vec<Mob> = stage.get(g).unwrap().submobjects().to_vec();
    assert_eq!(stage.get(children[0]).unwrap().buffer.len(), 4);
    assert_eq!(stage.get(children[1]).unwrap().buffer.len(), 1);
}

// Scenario 2: cross-group composition is edge surgery.
#[test]
fn s2_cross_group_composition() {
    let mut stage = Stage::new();
    let a = stage.add(square());
    let b = stage.add(square());
    let c = stage.add(square());
    let group1 = stage.add(Mobject::new());
    let group2 = stage.add(Mobject::new());
    stage.attach(group1, a).unwrap();
    stage.attach(group1, b).unwrap();
    stage.attach(group2, b).unwrap();
    stage.attach(group2, c).unwrap();
    assert_eq!(stage.family(group1), vec![group1, a, b]);
    assert_eq!(stage.family(group2), vec![group2, b, c]);
    stage.detach(group1, a);
    stage.attach(group2, a).unwrap();
    assert_eq!(stage.family(group1), vec![group1, b]);
    assert_eq!(stage.family(group2), vec![group2, b, c, a]);
}

// Scenario 3: multiple parents, diamond-safe traversal.
#[test]
fn s3_multiple_parents() {
    let mut stage = Stage::new();
    let shared = stage.add(square());
    let left = stage.add(Mobject::new());
    let right = stage.add(Mobject::new());
    let top = stage.add(Mobject::new());
    stage.attach(left, shared).unwrap();
    stage.attach(right, shared).unwrap();
    stage.attach(top, left).unwrap();
    stage.attach(top, right).unwrap();
    assert_eq!(stage.get(shared).unwrap().parents().len(), 2);
    let family = stage.family(top);
    assert_eq!(family.iter().filter(|m| **m == shared).count(), 1);
    assert_eq!(family.len(), 4);
}

// Scenario 4: removal from the scene with live handles.
#[test]
fn s4_removal_with_live_handles() {
    let mut stage = Stage::new();
    let sq = stage.add(square());
    stage.add_to_scene(sq).unwrap();
    stage.remove_from_scene(sq);
    assert!(stage.contains(sq));
    assert_eq!(stage.get(sq).unwrap().buffer.len(), 4);
    stage.add_to_scene(sq).unwrap();
    assert_eq!(stage.roots(), &[sq]);
}

// Scenario 5: the two-scene policy — stage-scoped handles, copy transfer.
#[test]
fn s5_two_scene_policy() {
    let mut stage_a = Stage::new();
    let mut stage_b = Stage::new();
    let sq = stage_a.add(square());
    assert!(!stage_b.contains(sq));
    assert_eq!(stage_b.add_to_scene(sq), Err(StageError::StaleHandle));
    let copied = stage_a.copy_into(sq, &mut stage_b).unwrap();
    assert!(stage_b.contains(copied));
    stage_b
        .get_mut(copied)
        .unwrap()
        .buffer
        .write(0, "point", &[9.0, 9.0, 9.0]);
    assert_ne!(
        stage_a.get(sq).unwrap().buffer.read(0, "point"),
        stage_b.get(copied).unwrap().buffer.read(0, "point")
    );
}

// Scenario 6: copy() — remapped internal references, independent data,
// updaters shared by reference.
#[test]
fn s6_copy_remapping() {
    let mut stage = Stage::new();
    let parent = stage.add(Mobject::new());
    let child = stage.add(square());
    stage.attach(parent, child).unwrap();
    stage.add_updater(child, |_, _| {}, false).unwrap();

    let copy = stage.copy_family(parent).unwrap();
    let copy_children: Vec<Mob> = stage.get(copy).unwrap().submobjects().to_vec();
    assert_eq!(copy_children.len(), 1);
    assert_ne!(copy_children[0], child);
    assert_eq!(stage.get(copy_children[0]).unwrap().parents(), &[copy]);
    stage
        .get_mut(child)
        .unwrap()
        .buffer
        .write(0, "point", &[7.0, 7.0, 7.0]);
    assert_ne!(
        stage.get(child).unwrap().buffer.read(0, "point"),
        stage.get(copy_children[0]).unwrap().buffer.read(0, "point")
    );
}

// Scenario 7: proxy identity across collection round-trips.
#[test]
fn s7_proxy_identity_across_collection() {
    let mut stage = Stage::new();
    let sq = stage.add(square());
    stage.add_to_scene(sq).unwrap();
    stage.pin(sq).unwrap();
    stage.remove_from_scene(sq);
    stage.add_to_scene(sq).unwrap();
    assert!(stage.contains(sq));
    stage.delete(sq).unwrap();
    assert!(stage.contains(sq), "delete must defer while pinned");
    stage.unpin(sq);
    assert!(!stage.contains(sq));
    assert_eq!(stage.try_get(sq).err(), Some(StageError::StaleHandle));
}

// Scenario 8: the §8.2 view protocol (V1–V6).
#[test]
fn s8_view_protocol() {
    let mut stage = Stage::new();
    let sq = stage.add(square());

    let entry = stage.get_mut(sq).unwrap();
    let rev0 = entry.buffer.revision();
    let view = entry.buffer.export_view(true);
    assert_eq!(entry.buffer.live_view_count(), 1);

    // V4: view writes are engine-visible and bump the revision.
    assert!(view.write(0, "point", &[5.0, 5.0, 0.0]));
    assert_eq!(entry.buffer.read(0, "point").unwrap(), vec![5.0, 5.0, 0.0]);
    assert!(entry.buffer.revision() > rev0);

    // V3: engine writes visible through the live view.
    entry.buffer.write(1, "point", &[6.0, 6.0, 0.0]);
    assert_eq!(view.read(1, "point").unwrap(), vec![6.0, 6.0, 0.0]);

    // V1/V3: copy-on-resize detaches; the old generation lives on.
    let pinned = view.read(0, "point").unwrap();
    entry.buffer.resize(8).unwrap();
    assert!(!view.is_attached_to(&entry.buffer));
    entry.buffer.write(0, "point", &[1.0, 2.0, 3.0]);
    assert_eq!(view.read(0, "point").unwrap(), pinned);
    assert_eq!(entry.buffer.read(7, "point").unwrap(), vec![0.0, 0.0, 0.0]);

    // V5: snapshots never share a generation with a live view.
    let fresh_view = entry.buffer.export_view(true);
    let snapshot = stage.snapshot();
    let entry = stage.get_mut(sq).unwrap();
    entry.buffer.write(2, "point", &[4.0, 4.0, 4.0]);
    assert_eq!(fresh_view.read(2, "point").unwrap(), vec![4.0, 4.0, 4.0]);
    stage.restore(&snapshot);
    assert_ne!(
        stage.get(sq).unwrap().buffer.read(2, "point").unwrap(),
        vec![4.0, 4.0, 4.0],
        "snapshot must not have seen the post-snapshot write"
    );
}

// Scenario 9: updater closures capturing handles.
#[test]
fn s9_updater_closures() {
    let mut stage = Stage::new();
    let follower = stage.add(square());
    let leader = stage.add(Mobject::from_points(&[[10.0, 0.0, 0.0]]));
    stage.add_to_scene(follower).unwrap();
    stage.add_to_scene(leader).unwrap();

    stage
        .add_dt_updater(
            follower,
            move |stage, me, dt| {
                let target = stage.get(leader).unwrap().buffer.read(0, "point").unwrap();
                let entry = stage.get_mut(me).unwrap();
                let current = entry.buffer.read(0, "point").unwrap();
                let step = dt as f32;
                entry.buffer.write(
                    0,
                    "point",
                    &[
                        current[0] + step * (target[0] - current[0]),
                        current[1] + step * (target[1] - current[1]),
                        current[2] + step * (target[2] - current[2]),
                    ],
                );
            },
            false,
        )
        .unwrap();
    let before = stage
        .get(follower)
        .unwrap()
        .buffer
        .read(0, "point")
        .unwrap();
    stage.update(0.5);
    let after = stage
        .get(follower)
        .unwrap()
        .buffer
        .read(0, "point")
        .unwrap();
    assert!(after[0] > before[0]);
    assert!((stage.time() - 0.5).abs() < 1e-12);

    // call_now runs exactly once (the Reference double-calls — fixed).
    let counter = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = std::rc::Rc::clone(&counter);
    stage
        .add_updater(follower, move |_, _| seen.set(seen.get() + 1), true)
        .unwrap();
    assert_eq!(counter.get(), 1);
}

// Scenario 10: snapshot/restore, CoW and exact.
#[test]
fn s10_snapshot_restore() {
    let mut stage = Stage::new();
    let a = stage.add(square());
    let b = stage.add(square());
    stage.add_to_scene(a).unwrap();
    stage.add_to_scene(b).unwrap();

    let snapshot = stage.snapshot();
    stage
        .get_mut(a)
        .unwrap()
        .buffer
        .write(0, "point", &[99.0, 0.0, 0.0]);
    stage.delete(b).unwrap();
    let c = stage.add(square());
    stage.add_to_scene(c).unwrap();

    stage.restore(&snapshot);
    assert_eq!(
        stage.get(a).unwrap().buffer.read(0, "point").unwrap(),
        vec![1.0, 1.0, 0.0]
    );
    assert!(stage.contains(b), "deleted entry restored");
    assert_eq!(stage.roots(), &[a, b]);
    assert!(
        !stage.contains(c),
        "post-snapshot handle stale after restore"
    );
}

// ------------------------------------------------- fm-ce8 acceptance extras

// Generational safety: stale handles are a defined error, and slot reuse
// can never leak a stranger's data through an old handle.
#[test]
fn generational_safety() {
    let mut stage = Stage::new();
    let old = stage.add(square());
    stage.delete(old).unwrap();
    assert_eq!(stage.try_get(old).err(), Some(StageError::StaleHandle));
    // The freed slot is reused with a bumped generation…
    let replacement = stage.add(Mobject::from_points(&[[42.0, 0.0, 0.0]]));
    // …so the stale handle still refuses to resolve.
    assert!(!stage.contains(old));
    assert!(stage.get(old).is_none());
    assert!(stage.contains(replacement));
    assert_eq!(stage.pin(old), Err(StageError::StaleHandle));
    assert_eq!(stage.delete(old), Err(StageError::StaleHandle));
    assert_eq!(stage.copy_family(old).err(), Some(StageError::StaleHandle));
}


// Rollback changes scene contents, not the identities already handed to callers.
#[test]
fn rollback_never_reissues_appended_or_recycled_handles() {
    let mut stage = Stage::new();
    let kept = stage.add(square());
    stage.add_to_scene(kept).unwrap();
    let freed = stage.add(square());
    stage.delete(freed).unwrap();
    let checkpoint = stage.snapshot();
    let before = stage.snapshot_bytes().unwrap();
    let mut discarded = vec![freed];

    for _ in 0..4 {
        let recycled = stage.add(square());
        let appended = stage.add(Mobject::from_points(&[[42.0, 0.0, 0.0]]));
        stage.add_to_scene(recycled).unwrap();
        stage.add_to_scene(appended).unwrap();
        for &old in &discarded {
            assert!(stage.get(old).is_none());
            assert_eq!(stage.pin(old), Err(StageError::StaleHandle));
            assert_eq!(stage.delete(old), Err(StageError::StaleHandle));
            assert_eq!(stage.copy_family(old).err(), Some(StageError::StaleHandle));
        }
        discarded.extend([recycled, appended]);
        stage.restore(&checkpoint);
        assert_eq!(stage.snapshot_bytes().unwrap(), before);
        assert_eq!(stage.roots(), &[kept]);
        assert!(discarded.iter().all(|&old| !stage.contains(old)));
    }
}

#[test]
fn restored_live_identity_does_not_reissue_its_discarded_replacements() {
    let mut stage = Stage::new();
    let original = stage.add(square());
    stage.add_to_scene(original).unwrap();
    let checkpoint = stage.snapshot();
    let before = stage.snapshot_bytes().unwrap();
    let mut discarded = Vec::new();

    for _ in 0..4 {
        stage.delete(original).unwrap();
        let replacement = stage.add(Mobject::new());
        assert!(discarded.iter().all(|&old| !stage.contains(old)));
        discarded.push(replacement);
        stage.restore(&checkpoint);
        assert!(stage.contains(original), "an explicit restore preserves the handle");
        assert!(discarded.iter().all(|&old| !stage.contains(old)));
        assert_eq!(stage.snapshot_bytes().unwrap(), before);
    }
}

#[test]
fn discarded_proxy_pins_and_live_views_cannot_touch_a_new_allocation() {
    let mut stage = Stage::new();
    let empty = stage.snapshot();
    let discarded = stage.add(square());
    stage.pin(discarded).unwrap();
    let view = stage.get_mut(discarded).unwrap().buffer.export_view(true);
    stage.delete(discarded).unwrap();
    assert!(stage.contains(discarded), "the proxy defers deletion");
    stage.restore(&empty);

    let replacement = stage.add(Mobject::from_points(&[[42.0, 0.0, 0.0]]));
    stage.pin(replacement).unwrap();
    stage.delete(replacement).unwrap();
    stage.unpin(discarded);
    assert_eq!(stage.get(replacement).unwrap().pins(), 1);
    assert_eq!(stage.delete(discarded), Err(StageError::StaleHandle));
    assert!(view.write(0, "point", &[99.0, 0.0, 0.0]));
    assert_eq!(
        stage.get(replacement).unwrap().buffer.read(0, "point").unwrap(),
        [42.0, 0.0, 0.0]
    );
    stage.unpin(replacement);
    assert!(!stage.contains(replacement));
}

#[test]
fn durable_rollback_preserves_bytes_without_reissuing_discarded_handles() {
    let mut stage = Stage::new();
    let kept = stage.add(square());
    stage.add_to_scene(kept).unwrap();
    let bytes = stage.snapshot_bytes().unwrap();
    let discarded = stage.add(square());
    let decoded = fmn_mobject::Snapshot::from_bytes(&bytes, &stage).unwrap();

    stage.restore(&decoded.snapshot);
    assert_eq!(stage.snapshot_bytes().unwrap(), bytes);
    let replacement = stage.add(Mobject::new());
    assert_ne!(replacement, discarded);
    assert!(stage.get(discarded).is_none());
    assert!(stage.contains(kept));
}

#[test]
fn materialized_forks_mint_deterministically_without_sharing_rollback_history() {
    let mut source = Stage::new();
    let kept = source.add(Mobject::new());
    let checkpoint = source.snapshot();
    let mut left = checkpoint.materialize();
    let mut right = checkpoint.materialize();
    let first_left = left.add(square());
    let first_right = right.add(square());
    let first_updater_left = left.add_updater(kept, |_, _| {}, false).unwrap();
    let first_updater_right = right.add_updater(kept, |_, _| {}, false).unwrap();
    assert_eq!(first_left, first_right);
    assert_eq!(first_updater_left, first_updater_right);
    assert_eq!(left.snapshot_bytes().unwrap(), right.snapshot_bytes().unwrap());

    left.restore(&checkpoint);
    let second_left = left.add(square());
    let second_updater_left = left.add_updater(kept, |_, _| {}, false).unwrap();
    assert_ne!(second_left, first_left);
    assert_ne!(second_updater_left, first_updater_left);
    assert!(right.contains(first_right));
    assert_eq!(right.updater_ids(kept), [first_updater_right]);

    right.restore(&checkpoint);
    assert_eq!(right.add(square()), second_left);
    assert_eq!(
        right.add_updater(kept, |_, _| {}, false).unwrap(),
        second_updater_left
    );
    assert_eq!(left.snapshot_bytes().unwrap(), right.snapshot_bytes().unwrap());
    assert_eq!(source.add(square()), first_left, "forks do not advance the source");
}

#[test]
fn restoring_another_branch_observes_both_live_and_deleted_generations() {
    let mut stage = Stage::new();
    let empty = stage.snapshot();
    let mut branch = empty.materialize();
    let mut history = Vec::new();
    for _ in 0..4 {
        let mob = branch.add(square());
        history.push(mob);
        branch.delete(mob).unwrap();
    }
    let branch_live = branch.add(square());
    history.push(branch_live);
    let live_checkpoint = branch.snapshot();
    stage.restore(&live_checkpoint);
    assert!(stage.contains(branch_live));
    stage.restore(&empty);
    let replacement = stage.add(Mobject::new());
    assert!(history.iter().all(|&old| old != replacement && !stage.contains(old)));

    branch.delete(branch_live).unwrap();
    let deleted_checkpoint = branch.snapshot();
    let mut deleted_fork = deleted_checkpoint.materialize();
    deleted_fork.restore(&empty);
    let next_in_fork = deleted_fork.add(Mobject::new());
    assert!(history.iter().all(|&old| old != next_in_fork));
    stage.restore(&deleted_checkpoint);
    stage.restore(&empty);
    let next_in_stage = stage.add(Mobject::new());
    assert_ne!(next_in_stage, replacement);
    assert!(history.iter().all(|&old| old != next_in_stage));
}

#[test]
fn discarded_updater_removal_tokens_cannot_remove_new_registrations() {
    use std::cell::Cell;
    use std::rc::Rc;

    let mut stage = Stage::new();
    let mob = stage.add(Mobject::new());
    let kept = stage.add_updater(mob, |_, _| {}, false).unwrap();
    let checkpoint = stage.snapshot();
    let before = stage.snapshot_bytes().unwrap();
    let mut discarded = Vec::new();
    for _ in 0..4 {
        let calls = Rc::new(Cell::new(0));
        let seen = Rc::clone(&calls);
        let current = stage
            .add_updater(mob, move |_, _| seen.set(seen.get() + 1), false)
            .unwrap();
        for &old in &discarded {
            stage.remove_updater(mob, old);
        }
        assert_eq!(stage.updater_ids(mob), [kept, current]);
        stage.update_mobject(mob, 0.0);
        assert_eq!(calls.get(), 1);
        discarded.push(current);
        stage.restore(&checkpoint);
        assert_eq!(stage.updater_ids(mob), [kept]);
        assert_eq!(stage.snapshot_bytes().unwrap(), before);
    }
}

#[test]
fn imported_updater_cursors_survive_a_later_rollback() {
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::new());
    let checkpoint = stage.snapshot();
    let mut branch = checkpoint.materialize();
    let mut discarded = Vec::new();
    for _ in 0..4 {
        let id = branch.add_updater(mob, |_, _| {}, false).unwrap();
        branch.remove_updater(mob, id);
        discarded.push(id);
    }
    let imported = branch.snapshot();
    stage.restore(&imported);
    stage.restore(&checkpoint);
    assert_eq!(stage.snapshot_bytes().unwrap(), checkpoint.to_bytes().unwrap());
    let current = stage.add_updater(mob, |_, _| {}, false).unwrap();
    for old in discarded {
        assert_ne!(current, old);
        stage.remove_updater(mob, old);
    }
    assert_eq!(stage.updater_ids(mob), [current]);
}

#[test]
fn stateful_and_fallible_callbacks_receive_their_post_rollback_identity() {
    use fmn_mobject::UpdaterError;

    let mut stage = Stage::new();
    let mob = stage.add(Mobject::new());
    let checkpoint = stage.snapshot();
    let discarded = stage.add_updater(mob, |_, _| {}, false).unwrap();
    stage.restore(&checkpoint);

    let stateful = stage
        .add_dt_updater_with_state(
            mob,
            40_u32,
            |_, _| {},
            |stage, me, id, _| *stage.updater_state_mut::<u32>(me, id).unwrap() += 1,
            true,
        )
        .unwrap();
    assert_ne!(stateful, discarded);
    assert_eq!(stage.updater_state::<u32>(mob, stateful), Some(&41));
    stage.restore(&checkpoint);

    let expected = UpdaterError::callback("rollback identity", "refused");
    let callback_error = expected.clone();
    let non_dt = stage
        .add_try_updater(mob, move |_, _| Err(callback_error.clone()), false)
        .unwrap();
    stage.update_mobject(mob, 0.0);
    let failure = stage.take_updater_error().unwrap().unwrap();
    assert_eq!(failure.updater, non_dt);
    assert_eq!(failure.error, expected);
    assert_ne!(non_dt, stateful);
    stage.restore(&checkpoint);

    let dt = stage
        .add_try_dt_updater(
            mob,
            |_, _, _| Err(UpdaterError::callback("dt identity", "refused")),
            false,
        )
        .unwrap();
    stage.update_mobject(mob, 0.25);
    assert_eq!(stage.take_updater_error().unwrap().unwrap().updater, dt);
    assert_ne!(dt, non_dt);
    stage.restore(&checkpoint);

    let fallible_state = stage
        .add_try_dt_updater_with_state(
            mob,
            7_u32,
            |_, _| {},
            |stage, me, id, _| {
                assert_eq!(stage.updater_state::<u32>(me, id), Some(&7));
                Err(UpdaterError::callback("state identity", "refused"))
            },
            false,
        )
        .unwrap();
    stage.update_mobject(mob, 0.5);
    assert_eq!(
        stage.take_updater_error().unwrap().unwrap().updater,
        fallible_state
    );
    assert_ne!(fallible_state, dt);
}

// Snapshot cost is O(touched), not O(scene): untouched entries share
// storage with the snapshot; only written entries diverge.
#[test]
fn snapshot_cost_is_o_touched() {
    let mut stage = Stage::new();
    let mobs: Vec<Mob> = (0..16).map(|_| stage.add(square())).collect();
    for &m in &mobs {
        let child = stage.add(square());
        stage.attach(m, child).unwrap();
        stage.add_updater(m, |_, _| {}, false).unwrap();
    }
    let ids_before: Vec<usize> = mobs
        .iter()
        .map(|m| stage.get(*m).unwrap().buffer.storage_id())
        .collect();
    let meta_before: Vec<(usize, usize, usize, usize)> = mobs
        .iter()
        .map(|m| stage.get(*m).unwrap().metadata_storage_id())
        .collect();

    let snapshot = stage.snapshot();
    // Taking the snapshot copied nothing (no live views anywhere) and shared
    // CoW metadata vectors across the snapshot boundary.
    for ((m, id), meta) in mobs.iter().zip(&ids_before).zip(&meta_before) {
        assert_eq!(stage.get(*m).unwrap().buffer.storage_id(), *id);
        assert_eq!(stage.get(*m).unwrap().metadata_storage_id(), *meta);
    }

    // Touch exactly one entry's buffer and metadata.
    stage
        .get_mut(mobs[3])
        .unwrap()
        .buffer
        .write(0, "point", &[5.0, 5.0, 5.0]);
    let extra_child = stage.add(square());
    stage.attach(mobs[3], extra_child).unwrap();
    stage.add_updater(mobs[3], |_, _| {}, false).unwrap();
    for (i, ((m, id), meta)) in mobs.iter().zip(&ids_before).zip(&meta_before).enumerate() {
        let now = stage.get(*m).unwrap().buffer.storage_id();
        let meta_now = stage.get(*m).unwrap().metadata_storage_id();
        if i == 3 {
            assert_ne!(now, *id, "touched entry must have unshared");
            assert_ne!(meta_now.0, meta.0, "touched submobjects must have unshared");
            assert_ne!(meta_now.2, meta.2, "touched updaters must have unshared");
        } else {
            assert_eq!(now, *id, "untouched entry must still share");
            assert_eq!(meta_now, *meta, "untouched entry metadata must still share");
        }
    }

    let restored = snapshot.materialize();
    for (m, meta) in mobs.iter().zip(&meta_before) {
        assert_eq!(restored.get(*m).unwrap().metadata_storage_id(), *meta);
    }
}

// The family cache invalidates on structural change, transitively through
// ancestors, and never returns stale flattenings.
#[test]
fn family_cache_invalidation() {
    let mut stage = Stage::new();
    let root = stage.add(Mobject::new());
    let mid = stage.add(Mobject::new());
    let leaf1 = stage.add(square());
    stage.attach(root, mid).unwrap();
    stage.attach(mid, leaf1).unwrap();
    // Prime the caches at every level.
    assert_eq!(stage.family(root).len(), 3);
    assert_eq!(stage.family(mid).len(), 2);

    // A structural change deep in the tree invalidates ancestors.
    let leaf2 = stage.add(square());
    stage.attach(mid, leaf2).unwrap();
    assert_eq!(stage.family(mid), vec![mid, leaf1, leaf2]);
    assert_eq!(stage.family(root), vec![root, mid, leaf1, leaf2]);

    stage.detach(mid, leaf1);
    assert_eq!(stage.family(root), vec![root, mid, leaf2]);

    // Deletion invalidates through parents as well.
    stage.delete(leaf2).unwrap();
    assert_eq!(stage.family(root), vec![root, mid]);
}

// Cycles are refused with a typed error (the Reference recurses forever).
#[test]
fn cycle_attach_is_refused() {
    let mut stage = Stage::new();
    let a = stage.add(Mobject::new());
    let b = stage.add(Mobject::new());
    let c = stage.add(Mobject::new());
    stage.attach(a, b).unwrap();
    stage.attach(b, c).unwrap();
    assert_eq!(stage.attach(a, a), Err(StageError::CycleDetected));
    assert_eq!(stage.attach(c, a), Err(StageError::CycleDetected));
    // The failed attach left no partial edges behind.
    assert_eq!(stage.family(a), vec![a, b, c]);
    assert!(stage.get(a).unwrap().parents().is_empty());
}

#[test]
fn family_checkpoint_restores_shared_topology_and_content_without_arena_rollback() {
    let mut stage = Stage::new();
    let root = stage.add(Mobject::new());
    let left = stage.add(Mobject::new());
    let right = stage.add(Mobject::new());
    let shared = stage.add(square());
    let outside = stage.add(Mobject::new());
    stage.attach(root, left).unwrap();
    stage.attach(root, right).unwrap();
    stage.attach(left, shared).unwrap();
    stage.attach(right, shared).unwrap();
    stage.attach(outside, shared).unwrap();
    stage.add_to_scene(root).unwrap();
    stage.add_to_scene(outside).unwrap();
    stage.set_animating_status(shared, true, false);
    stage.suspend_updating(shared, false);
    stage.get_mut(shared).unwrap().uniforms_mut().depth_test = true;
    let storage = stage.get(shared).unwrap().buffer.storage_id();
    let saved = stage.checkpoint_family(root).unwrap();
    assert_eq!(
        stage.get(shared).unwrap().buffer.storage_id(),
        storage,
        "CoW capture"
    );

    // The new layout has opposite edges between old members. Restoring each
    // saved edge without first detaching changed edges would transiently cycle.
    stage.replace_children(root, &[shared]).unwrap();
    stage.replace_children(left, &[]).unwrap();
    stage.replace_children(right, &[]).unwrap();
    stage.attach(shared, right).unwrap();
    stage.replace_children(right, &[left]).unwrap();
    let newly_authored = stage.add(square());
    stage.attach(outside, newly_authored).unwrap();
    stage.add_to_scene(newly_authored).unwrap();
    let roots = stage.roots().to_vec();
    stage.set_time_from_clock(73.25);
    stage.shift(newly_authored, [9.0, 0.0, 0.0]);
    stage.get_mut(shared).unwrap().buffer =
        fmn_mobject::RecordBuffer::new(fmn_mobject::RecordSchema::vmobject(), 1).unwrap();
    stage.get_mut(shared).unwrap().uniforms_mut().depth_test = false;
    stage.set_animating_status(shared, false, false);
    stage.resume_updating(shared, false, false);
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = std::rc::Rc::clone(&calls);
    stage
        .add_updater(shared, move |_, _| counter.set(counter.get() + 1), false)
        .unwrap();

    stage.restore_family_checkpoint(&saved).unwrap();
    assert_eq!(stage.family(root), [root, left, shared, right]);
    assert_eq!(stage.get(left).unwrap().submobjects(), &[shared]);
    assert_eq!(stage.get(right).unwrap().submobjects(), &[shared]);
    assert!(stage.get(shared).unwrap().parents().contains(&outside));
    assert_eq!(
        stage.get(outside).unwrap().submobjects(),
        &[shared, newly_authored]
    );
    assert_eq!(stage.roots(), roots);
    assert_eq!(stage.time(), 73.25);
    assert_eq!(stage.get_center(newly_authored)[0], 9.0);
    assert_eq!(stage.get(shared).unwrap().buffer.len(), 4);
    assert_eq!(
        stage.get(shared).unwrap().buffer.schema(),
        &fmn_mobject::RecordSchema::mobject()
    );
    assert!(stage.get(shared).unwrap().uniforms().depth_test);
    assert!(stage.is_animating(shared));
    assert!(stage.is_updating_suspended(shared));
    stage.resume_updating(shared, false, true);
    assert_eq!(
        calls.get(),
        1,
        "updater registered after capture is preserved"
    );
}

#[test]
fn family_checkpoint_refuses_stale_and_foreign_handles_before_changing_anything() {
    let mut stage = Stage::new();
    let root = stage.add(Mobject::new());
    let child = stage.add(square());
    stage.attach(root, child).unwrap();
    let saved = stage.checkpoint_family(root).unwrap();
    stage.delete(child).unwrap();
    let replacement = stage.add(square());
    stage.attach(root, replacement).unwrap();
    stage.shift(replacement, [4.0, 0.0, 0.0]);
    assert_eq!(
        stage.restore_family_checkpoint(&saved),
        Err(StageError::StaleHandle)
    );
    assert_eq!(stage.family(root), [root, replacement]);
    assert_eq!(stage.get_center(replacement)[0], 4.0);

    let mut other = Stage::new();
    let foreign = other.add(square());
    assert_eq!(
        other.restore_family_checkpoint(&saved),
        Err(StageError::StaleHandle)
    );
    assert_eq!(other.get_center(foreign), [0.0, 0.0, 0.0]);
}

#[test]
fn family_checkpoint_detaches_live_views_and_does_not_retain_future_children() {
    let mut stage = Stage::new();
    let root = stage.add(square());
    let saved = stage.checkpoint_family(root).unwrap();
    let view = stage.get_mut(root).unwrap().buffer.export_view(true);
    assert!(view.write(0, "point", &[40.0, 40.0, 0.0]));
    let extra = stage.add(square());
    stage.attach(root, extra).unwrap();
    stage.restore_family_checkpoint(&saved).unwrap();
    assert_eq!(
        stage.get(root).unwrap().buffer.read(0, "point"),
        Some(vec![1.0, 1.0, 0.0])
    );
    assert!(stage.get(root).unwrap().submobjects().is_empty());
    assert!(
        stage.contains(extra),
        "content restore never deletes arena identities"
    );
    assert!(view.write(0, "point", &[99.0, 99.0, 0.0]));
    assert_eq!(
        stage.get(root).unwrap().buffer.read(0, "point"),
        Some(vec![1.0, 1.0, 0.0])
    );
}
