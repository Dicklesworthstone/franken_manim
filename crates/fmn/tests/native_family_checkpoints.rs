#[path = "support/family_checkpoints.rs"]
mod family_checkpoints;

#[test]
fn durable_family_restoration_preserves_certified_frames_and_external_state() {
    let (comparisons, _) = family_checkpoints::verify().expect("durable family render acceptance");
    assert_eq!(comparisons, 3);
}
