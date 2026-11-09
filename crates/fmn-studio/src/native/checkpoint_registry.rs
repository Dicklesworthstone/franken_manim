//! Explicit callback registration for data-only native checkpoint continuation.

use std::rc::Rc;

use fmn_hash::Sha256;
use fmn_scene::studio_bridge::{Mob, Stage, UpdaterFn, UpdaterId, UpdaterKindTag, UpdaterManifest};
use fmn_scene::{IntegrationError, Scene};

use super::program::execution_error;
use crate::{ProtocolDigest, ServiceError, WorkerErrorCode};

#[derive(Clone)]
struct RegisteredUpdater {
    id: UpdaterId,
    kind: UpdaterKindTag,
    version: ProtocolDigest,
    binding: UpdaterFn,
}

/// Versioned native updater functions whose mutable state lives in the Stage.
///
/// Create a fresh registry during each scene factory invocation and pass it to
/// the native program before playback. Registration accepts ordinary function
/// pointers, so callbacks cannot retain a captured arena or mutable closure
/// state. Functions must read their inputs from the receiving Stage; ambient
/// mutable globals, unjournaled I/O and native `updater_state` attributes are
/// outside this checkpoint contract. Use durable records or trackers instead.
/// Cache revisions, storage addresses and physical arena identities are not
/// durable values and must not determine authored behavior.
/// The supplied version digest must change when a callback's behavior changes.
///
/// Registration never runs the callback. Copies of a mobject may share its
/// callback and removal identity, and removing a registration does not erase
/// its recipe from this registry. A clone shares these stateless functions,
/// just as an ordinary mobject copy does; it does not reconstruct another
/// factory's registrations.
#[derive(Clone, Default)]
pub struct NativeUpdaterRegistry {
    // Sorted by the original removal identity, not pointer address or target.
    entries: Vec<RegisteredUpdater>,
}

impl NativeUpdaterRegistry {
    /// An empty registry for one native scene factory invocation.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `callback(stage, receiver)` without an initial updater pass.
    ///
    /// # Errors
    /// An invalid target, exhausted registration identity, allocation refusal,
    /// or reuse of a registry across arenas with conflicting removal identities.
    pub fn register_no_dt(
        &mut self,
        stage: &mut Stage,
        mob: Mob,
        version: ProtocolDigest,
        callback: fn(&mut Stage, Mob),
    ) -> Result<UpdaterId, ServiceError> {
        self.entries.try_reserve(1).map_err(execution_error)?;
        let id = stage
            .add_updater(mob, callback, false)
            .map_err(execution_error)?;
        self.remember(stage, mob, id, UpdaterKindTag::NonDt, version)
    }

    /// Register `callback(stage, receiver, dt)` without an initial updater pass.
    ///
    /// # Errors
    /// As [`Self::register_no_dt`].
    pub fn register_dt(
        &mut self,
        stage: &mut Stage,
        mob: Mob,
        version: ProtocolDigest,
        callback: fn(&mut Stage, Mob, f64),
    ) -> Result<UpdaterId, ServiceError> {
        self.entries.try_reserve(1).map_err(execution_error)?;
        let id = stage
            .add_dt_updater(mob, callback, false)
            .map_err(execution_error)?;
        self.remember(stage, mob, id, UpdaterKindTag::Dt, version)
    }

    fn remember(
        &mut self,
        stage: &mut Stage,
        mob: Mob,
        id: UpdaterId,
        kind: UpdaterKindTag,
        version: ProtocolDigest,
    ) -> Result<UpdaterId, ServiceError> {
        let position = match self
            .entries
            .binary_search_by_key(&id.raw(), |entry| entry.id.raw())
        {
            Ok(_) => {
                stage.remove_updater(mob, id);
                return Err(execution_error(
                    "native checkpoint registry has a conflicting updater identity",
                ));
            }
            Err(position) => position,
        };
        let binding = stage
            .updater_binding(mob, id)
            .expect("registration without a callback pass retains its binding");
        self.entries.insert(
            position,
            RegisteredUpdater {
                id,
                kind,
                version,
                binding,
            },
        );
        Ok(id)
    }

    fn entry(&self, id: UpdaterId, kind: UpdaterKindTag) -> Option<&RegisteredUpdater> {
        let index = self
            .entries
            .binary_search_by_key(&id.raw(), |entry| entry.id.raw())
            .ok()?;
        let entry = &self.entries[index];
        (entry.kind == kind).then_some(entry)
    }

    /// Stable across fresh factory instances. Addresses and process-local arena
    /// identities never enter the checkpoint's callback-version fingerprint.
    pub(super) fn fingerprint(&self) -> ProtocolDigest {
        let mut hash = Sha256::new();
        hash.update(b"fmn-native-updater-registry-v1\0");
        hash.update(&(self.entries.len() as u64).to_le_bytes());
        for entry in &self.entries {
            hash.update(&entry.id.raw().to_le_bytes());
            hash.update(&[match entry.kind {
                UpdaterKindTag::NonDt => 0,
                UpdaterKindTag::Dt => 1,
            }]);
            hash.update(entry.version.as_bytes());
        }
        hash.finalize()
    }

    /// Admit only the complete live inventory and original registered callables.
    /// A matching id/kind alone cannot bless an opaque replacement closure.
    pub(super) fn supports(&self, stage: &Stage, manifest: &UpdaterManifest) -> bool {
        if stage.has_native_updater_state()
            || stage.has_padding_sources()
            || stage.has_rollback_identity_history()
            || stage.updater_error().is_some()
            || stage.updater_manifest() != *manifest
        {
            return false;
        }
        let Ok(identities) = manifest.identities(stage) else {
            return false;
        };
        identities.into_iter().all(|identity| {
            let Some(entry) = self.entry(identity.id, identity.kind) else {
                return false;
            };
            let Some(actual) = stage.updater_binding(identity.mob, identity.id) else {
                return false;
            };
            same_binding(&actual, &entry.binding)
        })
    }

    /// Rebind the manifest returned by this Scene's durable restore using only
    /// this fresh factory's registered function pointers. Scene validates and
    /// resolves its complete pending manifest before changing any updater list.
    pub(super) fn rebind(
        &self,
        scene: &mut Scene,
        manifest: &UpdaterManifest,
    ) -> Result<(), ServiceError> {
        if scene.stage().has_native_updater_state()
            || scene.stage().has_padding_sources()
            || scene.stage().has_rollback_identity_history()
            || scene.stage().updater_error().is_some()
        {
            return Err(rejected(
                "transient native attributes, padding links, rollback identities or failure cannot be resumed",
            ));
        }
        let identities = manifest
            .identities(scene.stage())
            .map_err(|error| rejected(error.to_string()))?;
        if identities
            .iter()
            .any(|identity| self.entry(identity.id, identity.kind).is_none())
        {
            return Err(rejected(
                "native checkpoint has an unregistered updater identity or kind",
            ));
        }
        if identities.is_empty() {
            return Ok(());
        }
        scene
            .rebind_updaters(|_mob, id, kind| {
                self.entry(id, kind)
                    .map(|entry| entry.binding.clone())
                    .ok_or_else(|| {
                        IntegrationError::new(
                            "native checkpoint",
                            "updater identity or kind was not registered by this factory",
                        )
                    })
            })
            .map_err(|error| rejected(error.to_string()))
    }
}

fn same_binding(left: &UpdaterFn, right: &UpdaterFn) -> bool {
    match (left, right) {
        (UpdaterFn::NonDt(left), UpdaterFn::NonDt(right)) => Rc::ptr_eq(left, right),
        (UpdaterFn::Dt(left), UpdaterFn::Dt(right)) => Rc::ptr_eq(left, right),
        _ => false,
    }
}

fn rejected(message: impl Into<String>) -> ServiceError {
    ServiceError::new(WorkerErrorCode::CheckpointRejected, message)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use fmn_mobject::Snapshot;
    use fmn_scene::{NullSceneSink, RuntimeConfig, SceneError};

    use super::*;
    use crate::protocol_digest;

    fn increment(stage: &mut Stage, mob: Mob) {
        stage.increment_tracker_value(mob, 1.0).unwrap();
    }

    fn increment_dt(stage: &mut Stage, mob: Mob, dt: f64) {
        stage.increment_tracker_value(mob, dt).unwrap();
    }

    fn registered_scene() -> (Scene, Mob, NativeUpdaterRegistry, [UpdaterId; 2]) {
        let mut scene = Scene::new(RuntimeConfig::default(), 71).unwrap();
        let mob = scene.stage_mut().add_value_tracker(2.0);
        scene.add(&[mob]).unwrap();
        let mut registry = NativeUpdaterRegistry::new();
        let first = registry
            .register_no_dt(
                scene.stage_mut(),
                mob,
                protocol_digest(b"increment-v1"),
                increment,
            )
            .unwrap();
        let second = registry
            .register_dt(
                scene.stage_mut(),
                mob,
                protocol_digest(b"increment-dt-v1"),
                increment_dt,
            )
            .unwrap();
        (scene, mob, registry, [first, second])
    }

    #[test]
    fn registrations_are_observation_free_and_fingerprints_ignore_arena_addresses() {
        let (mut first, first_mob, first_registry, first_ids) = registered_scene();
        let (second, second_mob, second_registry, second_ids) = registered_scene();
        assert_ne!(first_mob, second_mob);
        assert_eq!(first_ids, second_ids);
        assert_eq!(first.stage().tracker_value(first_mob), Some(2.0));
        assert_eq!(second.stage().tracker_value(second_mob), Some(2.0));
        assert_eq!(first_registry.fingerprint(), second_registry.fingerprint());
        assert!(!same_binding(
            &first
                .stage()
                .updater_binding(first_mob, first_ids[0])
                .unwrap(),
            &second
                .stage()
                .updater_binding(second_mob, second_ids[0])
                .unwrap(),
        ));

        let before = first.state_bytes().unwrap();
        let manifest = first.stage().updater_manifest();
        assert!(first_registry.supports(first.stage(), &manifest));
        assert!(first_registry.clone().supports(first.stage(), &manifest));
        assert_eq!(first.state_bytes().unwrap(), before);
        first.stage_mut().update(0.25);
        assert_eq!(first.stage().tracker_value(first_mob), Some(3.25));

        let mut different = NativeUpdaterRegistry::new();
        let mut stage = Stage::new();
        let mob = stage.add_value_tracker(2.0);
        different
            .register_no_dt(&mut stage, mob, protocol_digest(b"increment-v2"), increment)
            .unwrap();
        different
            .register_dt(
                &mut stage,
                mob,
                protocol_digest(b"increment-dt-v1"),
                increment_dt,
            )
            .unwrap();
        assert_ne!(first_registry.fingerprint(), different.fingerprint());
    }

    #[test]
    fn manifest_matches_persistence_and_admits_copied_targets_and_removed_ids() {
        let (mut scene, mob, registry, ids) = registered_scene();
        let shared = scene.stage().updater_binding(mob, ids[0]).unwrap();
        let copy = scene.stage_mut().copy_family(mob).unwrap();
        // The copy is deliberately detached: root-only admission is unsound.
        scene.stage_mut().remove_updater(mob, ids[0]);
        let manifest = scene.stage().updater_manifest();
        assert_eq!(manifest.entries.len(), 2);
        assert!(same_binding(
            &shared,
            &scene.stage().updater_binding(copy, ids[0]).unwrap(),
        ));
        assert!(registry.supports(scene.stage(), &manifest));
        let snapshot = scene.stage().snapshot().to_bytes().unwrap();
        assert_eq!(
            Snapshot::from_bytes(&snapshot, scene.stage())
                .unwrap()
                .updaters,
            manifest,
        );

        let mut incomplete = manifest.clone();
        incomplete.entries.pop();
        assert!(!registry.supports(scene.stage(), &incomplete));
        assert!(!registry.supports(scene.stage(), &UpdaterManifest::default()));
        scene.stage_mut().clear_updaters(mob, false);
        scene.stage_mut().clear_updaters(copy, false);
        assert!(registry.supports(scene.stage(), &UpdaterManifest::default()));
    }

    #[test]
    fn opaque_and_replaced_bindings_are_refused_even_with_matching_ids_and_kinds() {
        let (mut scene, mob, registry, ids) = registered_scene();
        let bytes = scene.stage().snapshot().to_bytes().unwrap();
        let decoded = Snapshot::from_bytes(&bytes, scene.stage()).unwrap();
        scene.stage_mut().restore(&decoded.snapshot);
        let identities = decoded.updaters.identities(scene.stage()).unwrap();
        let opaque = UpdaterFn::NonDt(Rc::new(RefCell::new({
            let mut captured = 0.0;
            move |stage: &mut Stage, mob: Mob| {
                captured += 1.0;
                stage.increment_tracker_value(mob, captured).unwrap();
            }
        })));
        scene
            .stage_mut()
            .restore_updater_bindings(vec![
                (identities[0], opaque),
                (
                    identities[1],
                    registry
                        .entry(ids[1], UpdaterKindTag::Dt)
                        .unwrap()
                        .binding
                        .clone(),
                ),
            ])
            .unwrap();
        assert_eq!(scene.stage().updater_manifest(), decoded.updaters);
        assert!(!registry.supports(scene.stage(), &decoded.updaters));

        let unknown = scene.stage_mut().add_value_tracker(1.0);
        scene
            .stage_mut()
            .add_updater(unknown, increment, false)
            .unwrap();
        assert!(!registry.supports(scene.stage(), &scene.stage().updater_manifest()));
        assert_eq!(scene.stage().tracker_value(mob), Some(2.0));
    }

    #[test]
    fn native_attributes_on_detached_or_removed_updaters_block_data_only_resume() {
        let mut stage = Stage::new();
        let mob = stage.add_value_tracker(2.0);
        let mut registry = NativeUpdaterRegistry::new();
        let id = registry
            .register_no_dt(&mut stage, mob, protocol_digest(b"increment-v1"), increment)
            .unwrap();
        assert!(!stage.has_native_updater_state());
        stage.set_updater_state(mob, id, 4_u64, |_, _| {}).unwrap();
        assert!(stage.has_native_updater_state());
        assert!(!registry.supports(&stage, &stage.updater_manifest()));
        stage.remove_updater(mob, id);
        assert!(stage.updater_manifest().entries.is_empty());
        assert!(stage.has_native_updater_state());
        assert!(!registry.supports(&stage, &stage.updater_manifest()));
        stage.delete(mob).unwrap();
        assert!(!stage.has_native_updater_state());
    }

    #[test]
    fn transient_padding_links_block_resume_but_affine_placements_do_not() {
        let mut stage = Stage::new();
        let mut buffer =
            fmn_mobject::RecordBuffer::new(fmn_mobject::RecordSchema::vmobject(), 1).unwrap();
        buffer.write(0, "point", &[1.0, 2.0, 0.0]);
        let mob = stage.add(fmn_mobject::Mobject::from_buffer(buffer));
        let registry = NativeUpdaterRegistry::new();
        let before = stage.get(mob).unwrap().placement().coefficients();
        stage.shift(mob, [0.125, -2.0, 0.5]);
        assert_ne!(stage.get(mob).unwrap().placement().coefficients(), before);
        assert!(registry.supports(&stage, &stage.updater_manifest()));
        assert!(!stage.has_padding_sources());
        stage.add_n_more_submobjects(mob, 1).unwrap();
        let padding = stage.get(mob).unwrap().submobjects()[0];
        assert_eq!(stage.padding_source(padding), Some(mob));
        assert!(stage.has_padding_sources());
        assert!(!registry.supports(&stage, &stage.updater_manifest()));
        stage.detach(mob, padding);
        stage.delete(mob).unwrap();
        assert!(
            stage.has_padding_sources(),
            "detached links remain observable"
        );
        assert!(!registry.supports(&stage, &stage.updater_manifest()));
        stage.delete(padding).unwrap();
        assert!(!stage.has_padding_sources());
        assert!(registry.supports(&stage, &stage.updater_manifest()));
    }

    #[test]
    fn discarded_rollback_identities_cannot_be_recreated_from_durable_bytes() {
        for discard_updater in [false, true] {
            let (mut scene, mob, registry, _) = registered_scene();
            let stage = scene.stage_mut();
            // Ordinary creation/deletion writes its high-water mark into the
            // durable free-slot ledger and remains safe to continue.
            let temporary = stage.add_value_tracker(7.0);
            stage.delete(temporary).unwrap();
            assert!(!stage.has_rollback_identity_history());
            assert!(registry.supports(stage, &stage.updater_manifest()));
            let saved = stage.snapshot();
            let canonical = saved.to_bytes().unwrap();
            if discard_updater {
                stage.add_updater(mob, increment, false).unwrap();
            } else {
                stage.add_value_tracker(8.0);
            }
            stage.restore(&saved);
            assert_eq!(stage.snapshot().to_bytes().unwrap(), canonical);
            assert!(stage.has_rollback_identity_history());
            assert!(!registry.supports(stage, &stage.updater_manifest()));
        }
    }

    #[test]
    fn fresh_function_registry_rebinds_restored_copies_without_an_updater_pass() {
        let (mut source, source_mob, source_registry, ids) = registered_scene();
        let copy = source.stage_mut().copy_family(source_mob).unwrap();
        source.add(&[copy]).unwrap();
        source.stage_mut().remove_updater(source_mob, ids[0]);
        source.wait(Some(0.1), &mut NullSceneSink).unwrap();
        source.rng_mut().next_u64();
        let bytes = source.state_bytes().unwrap();

        let (mut restored, restored_mob, registry, _) = registered_scene();
        assert!(!restored.stage().contains(source_mob));
        assert_eq!(source_registry.fingerprint(), registry.fingerprint());
        let receipt = restored.restore_state_bytes(&bytes).unwrap();
        assert!(restored.stage().updater_manifest().entries.is_empty());
        registry.rebind(&mut restored, &receipt.updaters).unwrap();
        assert_eq!(restored.state_bytes().unwrap(), bytes);
        assert!(registry.supports(restored.stage(), &receipt.updaters));
        assert_eq!(source.rng_mut().next_u64(), restored.rng_mut().next_u64());

        source.wait(Some(0.1), &mut NullSceneSink).unwrap();
        restored.wait(Some(0.1), &mut NullSceneSink).unwrap();
        assert_eq!(
            source.state_bytes().unwrap(),
            restored.state_bytes().unwrap()
        );
        assert_eq!(
            source.stage().tracker_value(source_mob),
            restored.stage().tracker_value(restored_mob),
        );
        let restored_copy = restored.mobjects()[1];
        assert_eq!(
            source.stage().tracker_value(copy),
            restored.stage().tracker_value(restored_copy),
        );
    }

    #[test]
    fn incomplete_or_wrong_kind_registry_refuses_before_installing_any_binding() {
        let (mut source, _, _, _) = registered_scene();
        let bytes = source.state_bytes().unwrap();
        let (mut candidate, mob, registry, _) = registered_scene();
        let receipt = candidate.restore_state_bytes(&bytes).unwrap();
        let mut incomplete = registry.clone();
        incomplete.entries.pop();
        assert_eq!(
            incomplete
                .rebind(&mut candidate, &receipt.updaters)
                .unwrap_err()
                .code,
            WorkerErrorCode::CheckpointRejected,
        );
        assert!(candidate.stage().updater_ids(mob).is_empty());
        assert!(matches!(
            candidate.state(),
            Err(SceneError::UnboundUpdaters)
        ));

        let mut wrong_kind = receipt.updaters.clone();
        wrong_kind.entries[0].1[0].1 = UpdaterKindTag::Dt;
        assert!(registry.rebind(&mut candidate, &wrong_kind).is_err());
        assert!(candidate.stage().updater_ids(mob).is_empty());
        registry.rebind(&mut candidate, &receipt.updaters).unwrap();
        assert_eq!(candidate.state_bytes().unwrap(), bytes);
    }
}
