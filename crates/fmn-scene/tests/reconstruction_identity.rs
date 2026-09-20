//! Renderer compatibility is not animation-law compatibility.
use fmn_anim::bundle::RECONSTRUCTION_LAW_VERSION;
use fmn_anim::{Timeline, prepare_animation};
use fmn_core::{rate, rng::RngRoot};
use fmn_hash::serial::Writer;
use fmn_mobject::Stage;
use fmn_mobject::animate::AnimateArgs;
use fmn_render::engine::EngineIdentity;
use fmn_scene::{
    BundleReadError, TIMELINE_BUNDLE_SCHEMA, TimelineBundle, bundle_engine_version,
    export_timeline_bundle,
};

#[test]
fn bundle_identity_binds_renderer_and_shared_reconstruction_law() {
    assert_eq!(RECONSTRUCTION_LAW_VERSION, 2);
    assert_eq!(
        bundle_engine_version(),
        format!(
            "{}:fmtl-law:2",
            EngineIdentity::certified().closure_string()
        )
    );
}

#[test]
fn legacy_and_mismatched_laws_refuse_before_later_fields_are_interpreted() {
    let renderer = EngineIdentity::certified().closure_string();
    for identity in [
        renderer.clone(),
        format!("{renderer}:fmtl-law:1"),
        format!("{renderer}:fmtl-law:3"),
    ] {
        let mut writer = Writer::new(TIMELINE_BUNDLE_SCHEMA);
        writer.put_str(&identity);
        let bytes = writer.finish().unwrap();
        match TimelineBundle::from_bytes(&bytes) {
            Err(BundleReadError::EngineMismatch { wanted, found }) => {
                assert_eq!(wanted, identity);
                assert_eq!(found, bundle_engine_version());
            }
            _ => panic!("incompatible law must fail before reading the intentionally absent fps"),
        }
    }
}

#[test]
fn native_tracker_exports_carry_the_law_that_reconstructs_their_state() {
    let mut stage = Stage::new();
    let tracker = stage.add_value_tracker(1.0);
    stage.add_to_scene(tracker).unwrap();
    let builder = tracker
        .animate()
        .set_anim_args(AnimateArgs {
            rate_func: Some(rate::linear),
            ..AnimateArgs::default()
        })
        .unwrap()
        .set_value(9.0)
        .unwrap();
    let animation = prepare_animation(builder, &mut stage).unwrap();
    let mut timeline = Timeline::new(4).unwrap();
    timeline.play(vec![animation]).unwrap();
    let bytes = export_timeline_bundle(timeline, &mut stage, &RngRoot::from_seed(0)).unwrap();
    let bundle = TimelineBundle::from_bytes(&bytes).unwrap();
    assert_eq!(bundle.engine_version(), bundle_engine_version());
    let frame = bundle.stage_at(1).unwrap();
    assert_eq!(frame.tracker_value(frame.roots()[0]), Some(5.0));
}
