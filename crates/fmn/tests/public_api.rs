use fmn::prelude::*;

use fmn::animation::{FrameMetadata, ImpureEffect, Purity};
use fmn::scene::{LifecycleEvent, LifecyclePhase};

#[derive(Default)]
struct PacketSink {
    captures: usize,
}

impl SceneSink for PacketSink {
    fn capture(
        &mut self,
        _reason: CaptureReason,
        _packet: fmn::animation::FramePacket,
    ) -> std::result::Result<(), IntegrationError> {
        self.captures += 1;
        Ok(())
    }
}

#[derive(Default)]
struct PublicScene {
    circle: Option<Mob>,
    square: Option<Mob>,
}

impl SceneConstruct for PublicScene {
    fn name(&self) -> &str {
        "public_api_circle_shift"
    }

    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let circle = stage.add(Circle::new().radius(0.75).color(BLUE_C))?;
        let square = stage.add(Square::new().side_length(1.25).color(YELLOW))?;
        self.circle = Some(circle);
        self.square = Some(square);

        let movement = circle
            .animate()
            .set_anim_args(AnimateArgs {
                run_time: Some(0.25),
                rate_func: Some(fmn::core::rate::linear),
                ..AnimateArgs::default()
            })?
            .shift([0.5, 0.25, 0.0])?;
        let square_movement = square
            .animate()
            .set_anim_args(AnimateArgs {
                run_time: Some(0.25),
                rate_func: Some(fmn::core::rate::linear),
                ..AnimateArgs::default()
            })?
            .shift([-0.25, 0.0, 0.0])?
            .rotate(PI / 4.0)?
            .set_opacity(0.5)?;
        stage.play((movement, square_movement))?;
        stage.wait(0.125)?;
        Ok(())
    }
}

#[test]
fn public_prelude_executes_the_real_scene_runtime() {
    let mut program = PublicScene::default();
    let mut sink = PacketSink::default();
    let completed = run_scene(
        &mut program,
        RuntimeConfig {
            fps: 8,
            ..RuntimeConfig::default()
        },
        0xFACADE,
        &mut sink,
    )
    .expect("public scene runs");

    assert_eq!(completed.report().play_count, 2);
    assert_eq!(sink.captures, 3, "two play frames plus one wait frame");
    let circle = program.circle.expect("construct stored its public handle");
    let square = program.square.expect("construct stored its square handle");
    assert!(completed.scene().stage().contains(circle));
    assert!(completed.scene().stage().contains(square));
    let center = completed.scene().stage().get_center(circle);
    let square_center = completed.scene().stage().get_center(square);
    assert!((center[0] - 0.5).abs() < 1e-9);
    assert!((center[1] - 0.25).abs() < 1e-9);
    assert!((square_center[0] + 0.25).abs() < 1e-9);
    for field in ["stroke_rgba", "fill_rgba"] {
        let rgba = completed
            .scene()
            .stage()
            .get(square)
            .and_then(|entry| entry.buffer.read_column(field))
            .expect("public square carries its vector style");
        assert!(rgba.iter().skip(3).step_by(4).all(|alpha| *alpha == 0.5));
    }
}

struct ForeignHandleScene {
    foreign: Mob,
}

impl SceneConstruct for ForeignHandleScene {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let _ = stage.play(show_creation(self.foreign))?;
        Ok(())
    }
}

#[test]
fn public_runner_preserves_the_typed_foreign_handle_refusal() {
    let mut other_stage = fmn::mobject::Stage::new();
    let foreign = other_stage.add(Circle::new());
    let mut program = ForeignHandleScene { foreign };
    let mut sink = PacketSink::default();

    let result = run_scene(&mut program, RuntimeConfig::default(), 1, &mut sink);
    assert!(result.is_err(), "foreign animation target unexpectedly ran");
    let error = result.err().expect("the asserted error is present");
    assert!(matches!(
        &error,
        fmn::Error::Animation(AnimError::StaleHandle(handle)) if *handle == foreign
    ));
    assert_eq!(error.kind(), ErrorKind::Scene);
    assert_eq!(sink.captures, 0);
}

#[test]
fn error_categories_match_the_cli_schema_rows() {
    let config = fmn::Error::from(fmn::config::ConfigError::Value {
        path: "camera.fps".to_owned(),
        message: "must be nonzero".to_owned(),
    });
    let capability = fmn::Error::from(fmn::platform::fetch::FetchError::CapabilityAbsent {
        url: "https://example.invalid/asset".to_owned(),
    });
    let scene = fmn::Error::from(fmn::geometry::GeomError::EmptyPath);
    let render_integration = fmn::Error::from(fmn::scene::SceneError::Integration(
        fmn::scene::IntegrationError::new("test-sink", "publication failed"),
    ));
    let render = fmn::Error::from(fmn::platform::fs::FsError::NotFound {
        path: "missing.png".into(),
    });
    let budget = fmn::Error::from(fmn::platform::fetch::FetchError::TooLarge {
        url: "https://example.invalid/large".to_owned(),
        limit: 16,
    });

    assert_eq!((config.kind().name(), config.kind().code()), ("config", 3));
    assert_eq!(
        (capability.kind().name(), capability.kind().code()),
        ("capability", 4)
    );
    assert_eq!((scene.kind().name(), scene.kind().code()), ("scene", 5));
    assert_eq!(
        (
            render_integration.kind().name(),
            render_integration.kind().code()
        ),
        ("render", 6)
    );
    assert_eq!((render.kind().name(), render.kind().code()), ("render", 6));
    assert_eq!((budget.kind().name(), budget.kind().code()), ("budget", 8));
}

#[derive(Default)]
struct EarlyEndScene;

impl SceneConstruct for EarlyEndScene {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        stage.end()
    }
}

#[test]
fn public_runner_preserves_normal_early_termination() {
    let mut sink = PacketSink::default();
    let completed = run_scene(&mut EarlyEndScene, RuntimeConfig::default(), 2, &mut sink)
        .expect("normal early termination is not an error");

    assert!(completed.report().ended_early);
    assert_eq!(completed.report().play_count, 0);
    assert_eq!(sink.captures, 0);
}

#[test]
fn public_prelude_constructs_expanded_mobjects() {
    let book = FontBook::bundled().expect("bundled fonts");

    let code = Code::new("fn main() {}").language("rust").build(&book);
    assert!(code.is_ok());

    let md = Markdown::new("# Header\n\nText").build(&book);
    assert!(md.is_ok());

    let table = TableMobject::from_csv("a,b\n1,2", ',')
        .unwrap()
        .build(&book);
    assert!(table.is_ok());

    let chart = BarChart::new(vec![1.0, 2.0, 3.0]).build(&book);
    assert!(chart.is_ok());

    let space = SampleSpace::new().build();
    assert!(space.is_ok());

    let graph = NetworkGraph::from_edge_list(&["a", "b"], &[("a", "b")])
        .laid_out(&GraphLayout::Circular)
        .unwrap()
        .build();
    assert!(graph.is_ok());

    let brace = Brace::new().build();
    assert!(!brace.points().is_empty());

    let dec = DecimalNumber::new(std::f64::consts::PI).build(&book);
    assert!(dec.is_ok());

    let int_mob = Integer::new(42.0).build(&book);
    assert!(int_mob.is_ok());

    let sphere = Sphere::new(1.0).build();
    assert!(!sphere.points().is_empty());

    let cube = Cube::new(1.0).build();
    assert!(!cube.children().is_empty());
}

#[test]
fn sound_cue_scene_records_an_exact_rational_request() {
    let mut program = fmn::builtins::sound_scene(fmn::builtins::SOUND_CUE_SCENE_NAME)
        .expect("the sound-cue scene is registered");
    let mut sink = PacketSink::default();
    let completed = run_scene(
        &mut program,
        RuntimeConfig {
            fps: 8,
            ..RuntimeConfig::default()
        },
        0,
        &mut sink,
    )
    .expect("the sound-cue scene runs");

    let requests = completed.scene().sound_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        request.sound_file,
        std::path::PathBuf::from(fmn::builtins::SOUND_CUE_ASSET_NAME)
    );
    assert_eq!(request.time.frames(), 4, "0.5 s at 8 fps");
    assert_eq!(request.time.fps(), 8);
    assert_eq!(request.time_offset, 0.0);
    assert_eq!(request.gain, None);
    assert_eq!(request.gain_to_background, None);

    assert!(fmn::builtins::sound_scene("circle_shift.v1").is_none());
    assert!(fmn::builtins::primitive_scene(fmn::builtins::SOUND_CUE_SCENE_NAME).is_none());
}

#[derive(Default)]
struct BoundarySink {
    metadata_only: bool,
    fail_capture: bool,
    attempts: usize,
    captures: Vec<(CaptureReason, FrameMetadata)>,
    packets: Vec<FramePacket>,
    events: Vec<LifecycleEvent>,
}

impl BoundarySink {
    fn record(
        &mut self,
        reason: CaptureReason,
        metadata: FrameMetadata,
    ) -> std::result::Result<(), IntegrationError> {
        self.attempts += 1;
        if self.fail_capture {
            return Err(IntegrationError::new("native-boundary", "capture refused"));
        }
        self.captures.push((reason, metadata));
        Ok(())
    }
}

impl SceneSink for BoundarySink {
    fn event(&mut self, event: LifecycleEvent) -> std::result::Result<(), IntegrationError> {
        self.events.push(event);
        Ok(())
    }

    fn capture_state_required(&self) -> bool {
        !self.metadata_only
    }

    fn accepts_pure_segments(&self) -> bool {
        // Predicate waits must still use the serial owner even when a sink
        // offers the compact route. Its default implementation refuses it.
        true
    }

    fn capture(
        &mut self,
        reason: CaptureReason,
        packet: FramePacket,
    ) -> std::result::Result<(), IntegrationError> {
        assert!(!self.metadata_only);
        self.record(reason, packet.metadata())?;
        self.packets.push(packet);
        Ok(())
    }

    fn capture_metadata(
        &mut self,
        reason: CaptureReason,
        metadata: FrameMetadata,
    ) -> std::result::Result<(), IntegrationError> {
        assert!(self.metadata_only);
        self.record(reason, metadata)
    }
}

#[derive(Default)]
struct ConditionalWaitScene {
    mob: Option<Mob>,
    observed: Vec<Vec3>,
}

impl SceneConstruct for ConditionalWaitScene {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let mob = stage.add(Mobject::from_points(&[ORIGIN]))?;
        self.mob = Some(mob);
        stage.add_dt_updater(mob, |arena, me, dt| arena.shift(me, [dt, 0.0, 0.0]), false)?;
        let report = stage.wait_until(1.0, |arena| {
            let center = arena.get_center(mob);
            self.observed.push(center);
            center[0] >= 0.375
        })?;
        assert_eq!(report.n_frames, 3);
        assert_eq!(report.base_frame, 0);
        assert!(matches!(
            report.purity,
            Purity::Stateful(ref effects) if effects.contains(&ImpureEffect::StopCondition)
        ));
        stage.add_updater(mob, |arena, me| arena.shift(me, [0.0, 0.5, 0.0]), false)?;
        stage.show()?;
        Ok(())
    }
}

#[test]
fn native_conditional_wait_and_show_use_the_active_sink_and_live_updaters() {
    let mut runs = Vec::new();
    for metadata_only in [false, true] {
        let mut program = ConditionalWaitScene::default();
        let mut sink = BoundarySink {
            metadata_only,
            ..BoundarySink::default()
        };
        let completed = run_scene(
            &mut program,
            RuntimeConfig {
                fps: 8,
                ..RuntimeConfig::default()
            },
            19,
            &mut sink,
        )
        .expect("conditional wait and explicit capture");
        assert_eq!(completed.report().play_count, 1);
        assert_eq!(completed.scene().time().frames(), 3);
        assert_eq!(
            program.observed,
            vec![[0.125, 0.0, 0.0], [0.25, 0.0, 0.0], [0.375, 0.0, 0.0]]
        );
        let mob = program.mob.unwrap();
        assert_eq!(completed.scene().stage().get_center(mob), [0.375, 0.5, 0.0]);
        assert_eq!(sink.captures.len(), 4);
        for (index, (reason, metadata)) in sink.captures.iter().take(3).enumerate() {
            assert_eq!(*reason, CaptureReason::Segment);
            assert_eq!(metadata.frame_index(), index as i64 + 1);
            assert_eq!(metadata.segment_frame(), index as i64 + 1);
            assert_eq!(metadata.time().fps(), 8);
        }
        let (reason, metadata) = sink.captures[3];
        assert_eq!(reason, CaptureReason::Show);
        assert_eq!(metadata.frame_index(), 3);
        assert_eq!(metadata.segment_frame(), 0);
        assert_eq!(metadata.alpha(), 1.0);
        if metadata_only {
            assert!(sink.packets.is_empty());
        } else {
            for (packet, expected) in sink.packets.iter().zip([
                [0.125, 0.0, 0.0],
                [0.25, 0.0, 0.0],
                [0.375, 0.0, 0.0],
                [0.375, 0.5, 0.0],
            ]) {
                assert_eq!(packet.materialize_stage().get_center(mob), expected);
            }
        }
        runs.push((sink.captures, sink.events));
    }
    assert_eq!(runs[0], runs[1], "packet demand cannot change scene timing");
}

#[derive(Default)]
struct FrameBoundScene {
    calls: usize,
}

impl SceneConstruct for FrameBoundScene {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        assert_eq!(
            stage
                .wait_until(0.0, |_| panic!("zero-time predicate"))?
                .n_frames,
            0
        );
        assert_eq!(
            stage
                .wait_until_frames(0, |_| panic!("zero-frame predicate"))?
                .n_frames,
            0
        );
        let mut initially_true = |_: &fmn::mobject::Stage| {
            self.calls += 1;
            true
        };
        assert_eq!(
            stage.wait_until_frames(43, &mut initially_true)?.n_frames,
            1
        );
        let report = stage.wait_until_frames(43, |_| {
            self.calls += 1;
            false
        })?;
        assert_eq!(report.base_frame, 1);
        assert_eq!(
            report.n_frames, 43,
            "43 / 24 seconds would round to 44 frames"
        );
        Ok(())
    }
}

#[test]
fn native_conditional_waits_keep_zero_bounds_and_exact_frame_budgets() {
    let mut program = FrameBoundScene::default();
    let mut sink = BoundarySink::default();
    let completed = run_scene(
        &mut program,
        RuntimeConfig {
            fps: 24,
            ..RuntimeConfig::default()
        },
        0,
        &mut sink,
    )
    .expect("bounded predicates");
    assert_eq!(program.calls, 44);
    assert_eq!(completed.report().play_count, 4);
    assert_eq!(completed.scene().time().frames(), 44);
    assert_eq!(sink.captures.len(), 44);
}

struct InvalidWaitScene;

impl SceneConstruct for InvalidWaitScene {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        for bound in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(matches!(
                stage.wait_until(bound, |_| panic!("invalid-time predicate")),
                Err(fmn::Error::Scene(SceneError::InvalidConfig(_)))
            ));
        }
        for error in [
            stage.wait_until(f64::MAX, |_| panic!("oversized-time predicate")),
            stage.wait_until_frames(u64::MAX, |_| panic!("oversized-frame predicate")),
        ] {
            assert!(matches!(
                error,
                Err(fmn::Error::Animation(AnimError::Clock(
                    fmn::animation::ClockError::RunTimeTooLong
                )))
            ));
        }
        assert_eq!(stage.scene().time().frames(), 0);
        assert_eq!(stage.scene().play_count(), 0);
        Ok(())
    }
}

#[test]
fn native_wait_admission_rejects_invalid_bounds_before_segment_lifecycle() {
    let mut sink = BoundarySink::default();
    run_scene(
        &mut InvalidWaitScene,
        RuntimeConfig::default(),
        0,
        &mut sink,
    )
    .expect("construction can handle a refused wait");
    assert_eq!(sink.attempts, 0);
    assert!(sink.events.iter().all(|event| event.segment.is_none()));
    assert!(
        sink.events
            .iter()
            .all(|event| event.phase != LifecyclePhase::Preflight)
    );
}

#[derive(Clone, Copy)]
enum NativeBoundary {
    Seconds,
    Frames,
    Show,
}

impl NativeBoundary {
    fn run(
        self,
        stage: &mut Stage<'_>,
        predicate: &mut dyn FnMut(&fmn::mobject::Stage) -> bool,
    ) -> fmn::Result<()> {
        match self {
            Self::Seconds => stage.wait_until(1.0, predicate).map(|_| ()),
            Self::Frames => stage.wait_until_frames(8, predicate).map(|_| ()),
            Self::Show => stage.show(),
        }
    }
}

struct BoundaryFailureScene {
    boundary: NativeBoundary,
    refuse_updater: bool,
    mob: Option<Mob>,
    predicate_calls: usize,
    completed: bool,
}

impl SceneConstruct for BoundaryFailureScene {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let mob = stage.add(Mobject::from_points(&[ORIGIN]))?;
        self.mob = Some(mob);
        if self.refuse_updater {
            stage.add_try_updater(
                mob,
                |_, _| Err(UpdaterError::callback("native-boundary", "update refused")),
                false,
            )?;
        }
        self.boundary.run(stage, &mut |_| {
            self.predicate_calls += 1;
            true
        })?;
        self.completed = true;
        Ok(())
    }
}

#[test]
fn native_waits_and_show_preserve_capture_and_updater_failures() {
    for boundary in [
        NativeBoundary::Seconds,
        NativeBoundary::Frames,
        NativeBoundary::Show,
    ] {
        for metadata_only in [false, true] {
            for refuse_updater in [false, true] {
                let mut program = BoundaryFailureScene {
                    boundary,
                    refuse_updater,
                    mob: None,
                    predicate_calls: 0,
                    completed: false,
                };
                let mut sink = BoundarySink {
                    metadata_only,
                    fail_capture: true,
                    ..BoundarySink::default()
                };
                let error = run_scene(&mut program, RuntimeConfig::default(), 0, &mut sink)
                    .err()
                    .expect("the boundary must preserve its failure");
                assert!(!program.completed);
                if refuse_updater {
                    assert!(matches!(
                        error,
                        fmn::Error::Animation(AnimError::Updater(ref failure))
                            if Some(failure.mobject) == program.mob
                                && failure.error == UpdaterError::callback("native-boundary", "update refused")
                    ));
                    assert_eq!(program.predicate_calls, 0);
                    assert_eq!(sink.attempts, 0);
                } else {
                    assert!(matches!(
                        error,
                        fmn::Error::Scene(SceneError::Integration(ref failure))
                            if failure.point() == "native-boundary"
                                && failure.message() == "capture refused"
                    ));
                    assert_eq!(sink.attempts, 1);
                }
            }
        }
    }
}
