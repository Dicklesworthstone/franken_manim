#[path = "support/native_bubbles.rs"]
mod native_bubbles;

#[test]
fn live_bubbles_match_detached_composition_across_render_teams() {
    let (comparisons, _) = native_bubbles::verify().expect("native bubble render acceptance");
    assert_eq!(comparisons, 18);
}
