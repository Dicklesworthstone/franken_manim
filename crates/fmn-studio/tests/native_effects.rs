//! Receipts describe the actual native prefix and verify that evidence on replay.

use fmn_anim::{AnimConfig, RateFunc, Transform};
use fmn_core::color::LinearRgba;
use fmn_mobject::{Mob, Mobject, RecordBuffer, RecordSchema};
use fmn_render::{
    EngineIdentity, FrameConfig, RetainedFrameRendererConfig, ScreenMap, Tiling, Viewport,
};
use fmn_scene::{
    EffectClass, Entry, EventPayload, ImpureEffectTag, Journal, Modifiers, PlayOverrides,
    RuntimeConfig, Scene,
};
use fmn_studio::native::{
    NativeReplayPolicy, NativeSceneProgram, NativeSceneWorker, NativeSegment, NativeWorkerConfig,
};
use fmn_studio::protocol::{StudioInput, studio_seek_command};
use fmn_studio::{
    JournalReplay, ServiceError, SupervisorRequest, WorkerErrorCode, WorkerResponse, WorkerService,
    protocol_digest,
};

const NAME: &str = "NativeEffects";

#[derive(Clone, Copy)]
enum Fixture {
    Pure,
    InitialUpdaters,
    TargetUpdater,
    SelfRemovingWait,
    Deferred,
}

fn initial() -> (Scene, Mob) {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 8,
            ..RuntimeConfig::default()
        },
        41,
    )
    .unwrap();
    let points = [
        [-1.0_f32, -1.0, 0.0],
        [0.0, -1.0, 0.0],
        [1.0, -1.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [-1.0, 1.0, 0.0],
        [-1.0, 0.0, 0.0],
        [-1.0, -1.0, 0.0],
    ];
    let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len()).unwrap();
    for (index, point) in points.iter().enumerate() {
        buffer.write(index, "point", point);
        buffer.write(index, "fill_rgba", &[0.25, 0.5, 1.0, 1.0]);
        buffer.write(index, "stroke_width", &[0.0]);
    }
    let root = scene.add_mobject(Mobject::from_buffer(buffer)).unwrap();
    (scene, root)
}

fn program(fixture: Fixture) -> Result<NativeSceneProgram, ServiceError> {
    let (mut scene, root) = initial();
    if matches!(fixture, Fixture::InitialUpdaters) {
        scene
            .stage_mut()
            .add_updater(root, |_, _| {}, false)
            .unwrap();
        scene
            .stage_mut()
            .add_dt_updater(root, |_, _, _| {}, false)
            .unwrap();
    }
    if matches!(fixture, Fixture::SelfRemovingWait) {
        scene
            .stage_mut()
            .add_dt_updater(
                root,
                |stage, mob, _| {
                    stage.clear_updaters(mob, false);
                },
                false,
            )
            .unwrap();
        return NativeSceneProgram::new(scene, vec![NativeSegment::WaitFrames { frames: 12 }], 12);
    }
    if matches!(fixture, Fixture::Deferred) {
        return NativeSceneProgram::new(
            scene,
            vec![
                NativeSegment::WaitFrames { frames: 4 },
                NativeSegment::edit(move |context| {
                    context
                        .stage_mut()
                        .add_dt_updater(
                            root,
                            |stage, mob, dt| {
                                stage.shift(mob, [dt, 0.0, 0.0]);
                            },
                            false,
                        )
                        .unwrap();
                    Ok(())
                }),
                NativeSegment::WaitFrames { frames: 4 },
                NativeSegment::edit(move |context| {
                    context.stage_mut().clear_updaters(root, false);
                    Ok(())
                }),
                NativeSegment::WaitFrames { frames: 4 },
            ],
            12,
        );
    }
    let target = scene.stage_mut().copy_family(root).unwrap();
    scene.stage_mut().shift(target, [2.0, 0.0, 0.0]);
    if matches!(fixture, Fixture::TargetUpdater) {
        scene
            .stage_mut()
            .add_dt_updater(target, |_, _, _| {}, false)
            .unwrap();
    }
    NativeSceneProgram::new(
        scene,
        vec![
            NativeSegment::Play {
                animations: vec![Box::new(Transform::new(root, target).with_config(
                    AnimConfig {
                        rate_func: RateFunc::linear(),
                        ..AnimConfig::default()
                    },
                ))],
                overrides: PlayOverrides {
                    run_time_frames: Some(8),
                    ..PlayOverrides::default()
                },
            },
            NativeSegment::WaitFrames { frames: 4 },
        ],
        12,
    )
}

fn config(replay: NativeReplayPolicy) -> NativeWorkerConfig {
    let renderer = RetainedFrameRendererConfig {
        frame: FrameConfig::new(
            Viewport {
                width: 32,
                height: 32,
            },
            ScreenMap {
                scale: 4.0,
                origin: [12.0, 16.0],
                y_up: true,
            },
            LinearRgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        ),
        tiling: Tiling {
            macro_tile: 32,
            fine_tile: 8,
        },
        engine: EngineIdentity::certified(),
        threads: 1,
    };
    let mut config = NativeWorkerConfig::new(
        NAME,
        protocol_digest(b"native-effects-build"),
        protocol_digest(b"native-effects-source"),
        13,
        8,
        renderer,
    );
    config.replay = replay;
    config.checkpoint_frames = 1;
    config
}

fn worker(fixture: Fixture) -> NativeSceneWorker {
    NativeSceneWorker::new(config(NativeReplayPolicy::ColdVerified), move || {
        program(fixture)
    })
    .unwrap()
}

fn entry(response: WorkerResponse) -> Entry {
    let WorkerResponse::JournalSegment { journal, .. } = response else {
        panic!("expected a native receipt");
    };
    let journal = Journal::from_bytes(&journal).unwrap();
    assert_eq!(journal.entries().len(), 1);
    journal.entries()[0].try_clone().unwrap()
}

fn commit(worker: &mut NativeSceneWorker, frame: i64) -> Entry {
    entry(
        worker
            .handle(SupervisorRequest::Play {
                scene: NAME.into(),
                command: studio_seek_command(NAME, frame).unwrap(),
            })
            .unwrap(),
    )
}

fn replay(
    worker: &mut NativeSceneWorker,
    journal: &Journal,
) -> Result<WorkerResponse, ServiceError> {
    worker.handle(SupervisorRequest::ReplayJournal(JournalReplay {
        scene: NAME.into(),
        from_entry: 0,
        through_entry: journal.entries().len() as u64,
        journal: journal.to_bytes().unwrap(),
    }))
}

#[test]
fn pure_native_receipts_and_actual_updater_reasons_replace_the_constant_label() {
    let mut pure = worker(Fixture::Pure);
    for frame in [0, 4, 8, 12] {
        let receipt = commit(&mut pure, frame);
        assert_eq!(receipt.effect, EffectClass::Pure);
        assert!(
            receipt
                .reads
                .iter()
                .any(|read| read.path == "native/effect-inventory-semantics")
        );
    }
    let mut initial = worker(Fixture::InitialUpdaters);
    assert_eq!(
        commit(&mut initial, 0).effect,
        EffectClass::Stateful(vec![
            ImpureEffectTag::DtUpdater,
            ImpureEffectTag::SceneUpdater
        ])
    );
    let mut target = worker(Fixture::TargetUpdater);
    assert_eq!(commit(&mut target, 0).effect, EffectClass::Pure);
    assert_eq!(
        commit(&mut target, 4).effect,
        EffectClass::Stateful(vec![ImpureEffectTag::DtUpdater])
    );

    // This callback erases its slot in the wait's initial zero-dt pass. Its
    // actual execution must still be part of every later prefix's evidence.
    let mut removing = worker(Fixture::SelfRemovingWait);
    assert_eq!(
        commit(&mut removing, 4).effect,
        EffectClass::Stateful(vec![ImpureEffectTag::DtUpdater])
    );
    assert!(
        !removing
            .program()
            .unwrap()
            .preview()
            .stage()
            .has_updaters_in_family(removing.program().unwrap().preview().stage().roots()[0])
    );
}

#[test]
fn prefix_effects_survive_removed_callbacks_and_match_incremental_and_backward_replay() {
    let mut original = worker(Fixture::Deferred);
    let mut journal = Journal::new();
    for frame in [4, 5, 12, 2, 12] {
        let receipt = commit(&mut original, frame);
        assert_eq!(
            receipt.effect,
            if frame <= 4 {
                EffectClass::Pure
            } else {
                EffectClass::Stateful(vec![
                    ImpureEffectTag::DtUpdater,
                    ImpureEffectTag::UnclassifiedAnimation,
                ])
            }
        );
        journal.record(receipt).unwrap();
    }
    let mut recovered = worker(Fixture::Deferred);
    let WorkerResponse::ReplayComplete { state_hashes, .. } =
        replay(&mut recovered, &journal).unwrap()
    else {
        panic!("expected verified replay");
    };
    assert_eq!(
        state_hashes,
        journal
            .entries()
            .iter()
            .map(|entry| entry.state_hash)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        recovered.program().unwrap().effect_class(),
        journal.entries().last().unwrap().effect
    );
    assert_eq!(recovered.journal_position(), 5);
}

#[test]
fn forged_effects_cannot_reuse_matching_state_hashes_or_install_a_partial_replay() {
    let mut original = worker(Fixture::Deferred);
    let mut journal = Journal::new();
    journal.record(commit(&mut original, 4)).unwrap();
    journal.record(commit(&mut original, 12)).unwrap();
    for altered in [0, 1] {
        let mut forged = Journal::new();
        for (index, receipt) in journal.entries().iter().enumerate() {
            let mut receipt = receipt.try_clone().unwrap();
            if index == altered {
                receipt.effect = if index == 0 {
                    EffectClass::Stateful(vec![ImpureEffectTag::DtUpdater])
                } else {
                    EffectClass::Pure
                };
            }
            forged.record(receipt).unwrap();
        }
        let mut recovered = worker(Fixture::Deferred);
        let error = replay(&mut recovered, &forged).unwrap_err();
        assert_eq!(error.code, WorkerErrorCode::ReplayFailed);
        assert!(error.message.contains("effects diverged"));
        assert_eq!(recovered.journal_position(), 0);
        assert_eq!(recovered.program().unwrap().frame_index(), 0);
        assert_eq!(
            recovered.program().unwrap().effect_class(),
            EffectClass::Pure
        );
        assert!(recovered.journal_tail().is_empty());
    }
}

#[test]
fn input_and_external_ingress_are_stateful_while_disabled_factories_remain_opaque() {
    let mut edited = worker(Fixture::Pure);
    let receipt = entry(
        edited
            .commit_input(StudioInput {
                frame: 0,
                revision: 0,
                target: None,
                event: EventPayload::MouseMotion {
                    point: [20.0, 20.0, 0.0],
                    delta: [0.0; 3],
                    modifiers: Modifiers::NONE,
                },
            })
            .unwrap(),
    );
    let stateful = EffectClass::Stateful(vec![ImpureEffectTag::UnclassifiedAnimation]);
    assert_eq!(receipt.effect, stateful);
    assert_eq!(commit(&mut edited, 8).effect, stateful);

    let (scene, _) = initial();
    let inbox = scene.event_inbox();
    let mut ingress =
        NativeSceneProgram::new(scene, vec![NativeSegment::WaitFrames { frames: 12 }], 12).unwrap();
    assert_eq!(ingress.effect_class(), stateful);
    drop(inbox);
    ingress.advance_to(4).unwrap();
    assert_eq!(ingress.effect_class(), stateful);

    let mut opaque = NativeSceneWorker::new(config(NativeReplayPolicy::Disabled), || {
        program(Fixture::Pure)
    })
    .unwrap();
    assert_eq!(commit(&mut opaque, 4).effect, EffectClass::Opaque);
}
