use super::*;
use fmn_scene::timeline_bundle::{BundleSoundtrack, attach_bundle_soundtrack};
use fmn_studio::WorkerService;

fn recording() -> Vec<u8> {
    let mut stage = fmn::mobject::Stage::new();
    let mut records = fmn::mobject::RecordBuffer::new(fmn::mobject::RecordSchema::vmobject(), 5).unwrap();
    records.write_range("point", 0, &[-1.0,-0.5,0.0, 0.0,1.0,0.0, 1.0,-0.5,0.0, 0.0,-0.5,0.0, -1.0,-0.5,0.0]);
    records.write_range("fill_rgba", 0, &[0.2,0.6,1.0,1.0].repeat(5));
    let mob = stage.add(fmn::mobject::Mobject::from_buffer(records));
    stage.add_to_scene(mob).unwrap();
    stage.add_dt_updater(mob, |stage, mob, dt| { stage.shift(mob, [dt,0.0,0.0]); }, false).unwrap();
    let mut timeline = fmn::animation::Timeline::new(8).unwrap();
    timeline.wait(0.5).unwrap();
    let pictures = fmn_scene::export_timeline_bundle(timeline, &mut stage, &fmn_core::rng::RngRoot::from_seed(7)).unwrap();
    let sound = BundleSoundtrack::new(10, 2, [0.25,-0.5].repeat(7)).unwrap();
    attach_bundle_soundtrack(&pictures, &sound, DEFAULT_MAX_BUNDLE_BYTES).unwrap()
}

fn command(extra: &[&str]) -> StudioCommand {
    let mut args = vec!["studio", "/recording.fmtl", "Lecture", "--no-browser", "--resolution", "32x24", "--threads", "1"];
    args.extend_from_slice(extra);
    let Invocation::Studio(command) = fmn_cli::parse_args(args).unwrap() else { panic!("Studio command"); };
    command
}

fn prepared() -> (VirtualFs, TimelineBundle, StudioRecordedAudio, Launch) {
    let fs = VirtualFs::new();
    let bytes = recording();
    fs.insert("/recording.fmtl", bytes.clone());
    let (bundle, audio) = StudioRecordedAudio::decode(SOURCE_ID, &bytes).unwrap();
    let audio = audio.unwrap();
    let launch = prepare_launch(&command(&[]), &bundle, &audio, &fs, PathBuf::from("/recording.fmtl")).unwrap();
    (fs, bundle, audio, launch)
}

#[test]
fn launch_roundtrip_binds_exact_settings_not_ambient_configuration_or_source_location() {
    let (_, _, _, launch) = prepared();
    let argv = launch.argv().unwrap();
    assert_eq!(argv[0], WORKER_ARG);
    let decoded = Launch::parse(&argv[1..].iter().map(OsString::from).collect::<Vec<_>>()).unwrap();
    assert_eq!(decoded.argv().unwrap(), argv);
    assert_eq!(decoded.build_id(), launch.build_id());
    let mut moved = launch.clone(); moved.source_path = PathBuf::from("/elsewhere/lecture.fmtl");
    assert_eq!(moved.build_id(), launch.build_id());
    for index in 0..5 {
        let mut changed = launch.clone();
        match index {
            0 => changed.source_digest = sha256(b"other recording").to_hex(),
            1 => changed.config_digest = sha256(b"other config").to_hex(),
            2 => changed.scene.push('X'),
            3 => changed.width += 1,
            _ => changed.seed += 1,
        }
        assert_ne!(changed.build_id(), launch.build_id());
    }
}

#[test]
fn source_replacement_is_refused_before_picture_audio_can_diverge() {
    let (fs, _, audio, launch) = prepared();
    let (pictures, loaded) = launch.read(&fs).unwrap();
    assert_eq!(pictures.fps(), 8);
    assert_eq!(loaded.info().source, audio.info().source);
    fs.insert("/recording.fmtl", b"changed".to_vec());
    assert!(launch.read(&fs).err().unwrap().message.contains("recording changed"));
}

#[test]
fn worker_argv_refuses_malformed_or_unbounded_inputs() {
    let (_, _, _, launch) = prepared();
    let args: Vec<OsString> = launch.argv().unwrap().into_iter().skip(1).map(OsString::from).collect();
    for (index, value) in [(0,"relative.fmtl"), (1,"bad"), (2,""), (3,"0"), (3,"4294967296"), (4,"4294967295"), (5,"0"), (6,"7ff8000000000000"), (7,"7ff0000000000000"), (11,"0"), (11,"4097"), (13,"bad")] {
        let mut invalid = args.clone(); invalid[index] = value.into();
        assert!(Launch::parse(&invalid).is_err(), "{index}: {value}");
    }
    assert!(Launch::parse(&args[..13]).is_err());
    let mut extra = args; extra.push("extra".into()); assert!(Launch::parse(&extra).is_err());
}

#[test]
fn native_worker_renders_and_inspects_original_clock_without_audio_in_its_picture_bundle() {
    let (_, pictures, audio, launch) = prepared();
    assert!(!pictures.has_soundtrack());
    let build = launch.build_id();
    let view = launch.view(&pictures).unwrap();
    let reads = CameraBundleWorker::input_reads(build, audio.source(), &pictures, &view).unwrap();
    assert!(reads.contains(audio.source()));
    let mut worker = CameraBundleWorker::new(launch.scene.clone(), build, audio.source().clone(), pictures, view).unwrap();
    worker.begin_session(build, fmn_studio::ProtocolLimits::default().max_frame_bytes).unwrap();
    let mut pixels = Vec::new();
    for frame in [0,3,0] {
        let WorkerResponse::Frame(picture) = worker.handle(SupervisorRequest::Scrub { scene: launch.scene.clone(), frame }).unwrap() else { panic!("picture"); };
        assert_eq!(picture.frame_index, u64::try_from(frame).unwrap());
        let fmn_studio::FramePayload::Pipe { bytes, .. } = picture.payload else { panic!("inline picture"); };
        pixels.push(bytes);
        let WorkerResponse::StudioData { bytes, .. } = worker.handle(SupervisorRequest::Inspect { scene: launch.scene.clone() }).unwrap() else { panic!("inspector"); };
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"fps\":8"));
        assert!(text.contains("\"frame_count\":4"));
    }
    assert_ne!(pixels[0], pixels[1]);
    assert_eq!(pixels[0], pixels[2]);
}

#[test]
fn explicit_clock_and_unsupported_modes_refuse_instead_of_silently_changing_replay() {
    let (fs, bundle, audio, _) = prepared();
    for extra in [vec!["--fps","9"], vec!["--reproducible"], vec!["--tui"], vec!["-n","1,2"], vec!["--engine","metal"]] {
        assert!(prepare_launch(&command(&extra), &bundle, &audio, &fs, PathBuf::from("/recording.fmtl")).is_err(), "{extra:?}");
    }
}
