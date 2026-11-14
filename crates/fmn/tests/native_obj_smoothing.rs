//! Generated OBJ normals survive material partitioning and the native pipeline.

#[path = "support/obj_smoothing.rs"]
mod obj_smoothing;

#[test]
fn smooth_obj_models_match_explicit_normals_in_native_animated_output() {
    let witness = obj_smoothing::verify().unwrap();
    assert_eq!(witness.frames, 30);
    assert_ne!(witness.first, witness.last);
}
