//! Live bubbles retain native content identity through drawing and lifecycle edits.

use std::cell::Cell;
use std::rc::Rc;

use fmn_core::constants::{BLUE, RED, UL, UR};
use fmn_core::rng::RngRoot;
use fmn_core::types::Vec3;
use fmn_library::{
    Bubble, BubbleConfig, BubbleDirection, BubbleError, DotCloud, Rectangle, SpeechBubble,
    ThoughtBubble, VMobject, VStyle, v_group,
};
use fmn_mobject::{Mob, Stage, StageError};

fn rectangle(stage: &mut Stage, width: f64, height: f64) -> Mob {
    stage.add(
        Rectangle::new()
            .width(width)
            .height(height)
            .build()
            .unwrap(),
    )
}

fn points(stage: &Stage, root: Mob) -> Vec<Vec<Vec3>> {
    stage
        .family(root)
        .into_iter()
        .map(|mob| stage.get_points(mob).unwrap_or_default())
        .collect()
}

fn close(actual: Vec3, expected: Vec3) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert!((actual - expected).abs() < 3e-6, "{actual} != {expected}");
    }
}

#[test]
fn construction_adopts_existing_live_content_and_matches_native_body_geometry() {
    let mut stage = Stage::new();
    let content = rectangle(&mut stage, 2.0, 1.0);
    stage.shift(content, [1.25, 0.75, 0.0]);
    stage.set_fill(content, Some(BLUE), Some(0.65), None, true);
    let content_before = points(&stage, content);
    let bubble = SpeechBubble::new()
        .add_to(&mut stage, Some(content))
        .unwrap();
    assert_eq!(bubble.content(), Some(content));
    assert_eq!(
        stage.get(bubble.mobject()).unwrap().submobjects(),
        [bubble.body(), content]
    );
    assert!(
        stage.roots().is_empty(),
        "construction does not change scene membership"
    );
    assert_eq!(points(&stage, content), content_before);
    assert_eq!(
        stage.get_fill_color(content).unwrap().to_rgb8(),
        BLUE.to_rgb8()
    );

    let detached_content = Rectangle::new()
        .width(2.0)
        .height(1.0)
        .build()
        .unwrap()
        .shifted([1.25, 0.75, 0.0]);
    let existing = fmn_library::drawings::speech_bubble_default(Some(&detached_content)).unwrap();
    let expected = stage.add(existing.children()[0].clone());
    let actual = points(&stage, bubble.body());
    let expected = points(&stage, expected);
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.len(), expected.len());
        for (&actual, expected) in actual.iter().zip(expected) {
            close(actual, expected);
        }
    }
}

#[test]
fn replacement_and_clear_detach_only_the_bubble_parent() {
    let mut stage = Stage::new();
    let first = rectangle(&mut stage, 3.0, 2.0);
    let other_parent = stage.add(v_group([]));
    stage.attach(other_parent, first).unwrap();
    let mut bubble = SpeechBubble::new().add_to(&mut stage, Some(first)).unwrap();
    stage.add_to_scene(bubble.mobject()).unwrap();
    let annotation = rectangle(&mut stage, 0.1, 0.1);
    stage.attach(bubble.mobject(), annotation).unwrap();
    let second = rectangle(&mut stage, 6.0, 3.0);
    let second_parent = stage.add(v_group([]));
    stage.attach(second_parent, second).unwrap();
    let before_roots = stage.roots().to_vec();
    let ratio = stage.get_width(second) / stage.get_height(second);
    assert_eq!(bubble.add_content(&mut stage, second).unwrap(), second);
    assert_eq!(bubble.content(), Some(second));
    assert_eq!(
        stage.get(bubble.mobject()).unwrap().submobjects(),
        [bubble.body(), second, annotation]
    );
    assert_eq!(stage.get(first).unwrap().parents(), [other_parent]);
    assert!(
        stage
            .get(second)
            .unwrap()
            .parents()
            .contains(&second_parent)
    );
    assert_eq!(stage.roots(), before_roots);
    assert!((stage.get_width(second) / stage.get_height(second) - ratio).abs() < 1e-9);
    close(
        stage.get_center(second),
        bubble.get_bubble_center(&stage).unwrap(),
    );
    assert_eq!(bubble.clear(&mut stage).unwrap(), Some(second));
    assert_eq!(bubble.clear(&mut stage).unwrap(), None);
    assert!(stage.contains(first) && stage.contains(second));
    assert_eq!(stage.get(second).unwrap().parents(), [second_parent]);
    assert_eq!(
        stage.get(bubble.mobject()).unwrap().submobjects(),
        [bubble.body(), annotation]
    );
    assert!(matches!(
        bubble.resize_to_content(&mut stage, 0.25),
        Err(BubbleError::NoContent)
    ));
    bubble
        .add_content_with_buff(&mut stage, first, 0.1)
        .unwrap();
    assert_eq!(bubble.content(), Some(first));
}

#[test]
fn fitting_current_content_uses_its_post_scale_family_bounds() {
    for shared_and_viewed in [false, true] {
        let mut stage = Stage::new();
        let leaf = rectangle(&mut stage, 2.0, 1.0);
        let content = if shared_and_viewed {
            let nested = stage.add(v_group([]));
            let root = stage.add(v_group([]));
            stage.attach(nested, leaf).unwrap();
            stage.attach(root, leaf).unwrap();
            stage.attach(root, nested).unwrap();
            root
        } else {
            leaf
        };
        let bubble = SpeechBubble::new()
            .add_to(&mut stage, Some(content))
            .unwrap();
        let body = stage.get_bounding_box(bubble.body());
        let expected_center = [
            body.mid[0],
            body.mid[1] + 0.125 * body.height(),
            body.mid[2],
        ];
        stage.scale(content, 10.0);
        let view = if shared_and_viewed {
            stage.bake_placement(leaf).unwrap();
            Some(
                stage
                    .get_mut(leaf)
                    .unwrap()
                    .buffer
                    .export_field_view("point", true)
                    .unwrap(),
            )
        } else {
            None
        };
        let topology = stage.topology_epoch();
        bubble
            .position_mobject_inside(&mut stage, content, 0.5)
            .unwrap();
        close(stage.get_center(content), expected_center);
        assert!((stage.get_width(content) - 1.0).abs() < 3e-6);
        assert!((stage.get_height(content) - 0.5).abs() < 3e-6);
        assert_eq!(stage.topology_epoch(), topology);
        if let Some(view) = view {
            assert!(view.is_attached_to(&stage.get(leaf).unwrap().buffer));
            let viewed: [f32; 3] = view.read(0, "point").unwrap().try_into().unwrap();
            close(viewed.map(f64::from), stage.get_points(leaf).unwrap()[0]);
        }
    }
}

#[test]
fn fitting_scales_root_dot_radii_without_dispatching_group_children() {
    for group_wrapper in [false, true] {
        let mut stage = Stage::new();
        let bubble = SpeechBubble::new().add_to(&mut stage, None).unwrap();
        let cloud =
            stage.add(DotCloud::new([[-10.0, -5.0, 0.0], [10.0, 5.0, 0.0]]).with_radius(2.0));
        let content = if group_wrapper {
            let group = stage.add(v_group([]));
            stage.attach(group, cloud).unwrap();
            group
        } else {
            cloud
        };
        bubble
            .position_mobject_inside(&mut stage, content, 0.5)
            .unwrap();
        let expected_radius = if group_wrapper { 2.0 } else { 0.1 };
        for radius in stage
            .get(cloud)
            .unwrap()
            .buffer
            .read_column("radius")
            .unwrap()
        {
            assert!((f64::from(radius) - expected_radius).abs() < 1e-6);
        }
        assert!((stage.get_width(content) - 1.0).abs() < 3e-6);
        assert!((stage.get_height(content) - 0.5).abs() < 3e-6);
    }
}

#[test]
fn pinning_flips_the_body_without_mirroring_content() {
    let mut stage = Stage::new();
    let content = stage.add(VMobject::from_points(vec![
        [-0.5, -0.25, 0.0],
        [-0.2, 0.6, 0.0],
        [0.75, 0.2, 0.0],
    ]));
    let mut bubble = SpeechBubble::new()
        .add_to(&mut stage, Some(content))
        .unwrap();
    let target = rectangle(&mut stage, 1.0, 2.0);
    stage.shift(target, [3.0, -1.5, 0.0]);
    let before = stage.get_points(content).unwrap();
    let before_center = stage.get_center(content);
    bubble.pin_to(&mut stage, target, true).unwrap();
    assert_eq!(bubble.direction(), BubbleDirection::Right);
    close(
        bubble.get_tip(&stage).unwrap(),
        stage.get_bounding_box_point(target, UL),
    );
    let after_center = stage.get_center(content);
    for (before, after) in before.into_iter().zip(stage.get_points(content).unwrap()) {
        close(
            std::array::from_fn(|axis| after[axis] - after_center[axis]),
            std::array::from_fn(|axis| before[axis] - before_center[axis]),
        );
    }
    bubble.move_tip_to(&mut stage, [-2.0, -2.5, 0.0]).unwrap();
    close(bubble.get_tip(&stage).unwrap(), [-2.0, -2.5, 0.0]);
    bubble.flip(&mut stage, false).unwrap();
    assert_eq!(bubble.direction(), BubbleDirection::Left);
}

#[test]
fn resize_preserves_body_handles_paint_uniforms_updaters_and_snapshots() {
    let mut stage = Stage::new();
    let content = rectangle(&mut stage, 1.0, 0.5);
    let mut bubble = SpeechBubble::new()
        .add_to(&mut stage, Some(content))
        .unwrap();
    let body = bubble.body();
    let family = stage.family(body);
    stage.set_fill(body, Some(RED), Some(0.4), Some(0.7), true);
    stage.set_stroke(body, Some(BLUE), Some(9.0), Some(0.6), Some(true), true);
    stage.get_mut(body).unwrap().uniforms_mut().depth_test = true;
    let calls = Rc::new(Cell::new(0));
    let updater_calls = Rc::clone(&calls);
    stage
        .add_updater(
            body,
            move |_, _| updater_calls.set(updater_calls.get() + 1),
            false,
        )
        .unwrap();
    let manifest = stage.updater_manifest();
    let old_points = points(&stage, body);
    let snapshot = stage.snapshot();
    stage.scale(content, 3.0);
    stage.shift(content, [1.5, 0.75, 0.0]);
    let content_before = points(&stage, content);
    bubble.resize_to_content(&mut stage, 0.5).unwrap();
    assert_eq!(stage.family(body), family);
    assert_eq!(bubble.body(), body);
    assert_eq!(points(&stage, content), content_before);
    assert_ne!(points(&stage, body), old_points);
    assert_eq!(points(&snapshot.materialize(), body), old_points);
    assert_eq!(stage.get_fill_color(body).unwrap().to_rgb8(), RED.to_rgb8());
    assert!((stage.get_fill_opacity(body).unwrap() - 0.4).abs() < 1e-6);
    assert_eq!(stage.get_stroke_width(body), Some(9.0));
    assert!(stage.get(body).unwrap().uniforms().depth_test);
    assert!(stage.get(body).unwrap().uniforms().stroke_behind);
    assert_eq!(stage.updater_manifest(), manifest);
    stage.update_mobject(bubble.mobject(), 0.0);
    assert_eq!(calls.get(), 1);
    let body_box = stage.get_bounding_box(body);
    let content_box = stage.get_bounding_box(content);
    assert!(body_box.min[0] <= content_box.min[0] - 0.49);
    assert!(body_box.max[0] >= content_box.max[0] + 0.49);
    assert!(body_box.max[1] >= content_box.max[1] + 0.49);
}

#[test]
fn thought_bubbles_rebuild_the_complete_family_with_repeatable_noise() {
    let mut stage = Stage::new();
    let content = rectangle(&mut stage, 1.0, 0.5);
    let mut rng = RngRoot::from_seed(19).substream("bubble").sequential();
    let before = rng.clone();
    let mut bubble = ThoughtBubble::new()
        .add_to(&mut stage, Some(content), &mut rng)
        .unwrap();
    assert_ne!(rng, before);
    let post_construction = rng.clone();
    let family = stage.family(bubble.body());
    let initial = points(&stage, bubble.body());
    bubble.resize_to_content(&mut stage, 0.1).unwrap();
    assert_eq!(points(&stage, bubble.body()), initial);
    stage.scale(content, 1.7);
    stage.set_fill(bubble.body(), Some(BLUE), Some(0.5), None, true);
    bubble.resize_to_content(&mut stage, 0.2).unwrap();
    assert_eq!(stage.family(bubble.body()), family);
    for &member in &family {
        assert_eq!(
            stage.get_fill_color(member).unwrap().to_rgb8(),
            BLUE.to_rgb8()
        );
    }
    let resized = points(&stage, bubble.body());
    bubble.resize_to_content(&mut stage, 0.2).unwrap();
    assert_eq!(points(&stage, bubble.body()), resized);
    assert_eq!(
        rng, post_construction,
        "resizing consumes no ambient scene randomness"
    );
}

#[test]
fn copied_bubbles_rebind_content_and_body_and_edit_independently() {
    let mut stage = Stage::new();
    let mut original = SpeechBubble::new().add_to(&mut stage, None).unwrap();
    stage.add_to_scene(original.mobject()).unwrap();
    let mut copy = original.copy(&mut stage).unwrap();
    assert_ne!(original.mobject(), copy.mobject());
    assert_ne!(original.body(), copy.body());
    assert_ne!(original.content(), copy.content());
    let untouched = points(&stage, original.mobject());
    copy.move_tip_to(&mut stage, [4.0, 1.0, 0.0]).unwrap();
    copy.clear(&mut stage).unwrap();
    assert_eq!(points(&stage, original.mobject()), untouched);
    assert_eq!(stage.roots(), [original.mobject()]);
    original.flip(&mut stage, true).unwrap();
    assert_eq!(copy.direction(), BubbleDirection::Left);
}

#[test]
fn explicit_svg_bodies_support_the_live_bubble_lifecycle() {
    let document = fmn_library::svg::SvgDocument::parse(
        br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 6">
            <rect x="0" y="0" width="10" height="4"/>
            <path d="M 1 4 L 1 6 L 4 4 Z"/>
        </svg>"#,
    )
    .unwrap();
    let mut stage = Stage::new();
    let content = rectangle(&mut stage, 2.0, 1.0);
    let mut bubble = Bubble::from_document(
        &mut stage,
        &document,
        Some(content),
        BubbleConfig {
            direction: BubbleDirection::Right,
            buff: 0.4,
            ..BubbleConfig::default()
        },
    )
    .unwrap();
    let family = stage.family(bubble.body());
    assert_eq!(family.len(), 3, "the SVG body retains both authored shapes");
    let initial = points(&stage, bubble.body());
    stage.set_fill(bubble.body(), Some(BLUE), Some(0.6), None, true);
    stage.scale(content, 2.0);
    bubble.resize_to_content(&mut stage, 0.75).unwrap();
    assert_eq!(stage.family(bubble.body()), family);
    assert_ne!(points(&stage, bubble.body()), initial);
    assert!((stage.get_width(bubble.body()) - 4.75).abs() < 3e-6);
    assert!((stage.get_height(bubble.body()) - 3.7125).abs() < 3e-6);
    for &member in &family {
        assert_eq!(
            stage.get_fill_color(member).unwrap().to_rgb8(),
            BLUE.to_rgb8()
        );
    }
    let target = rectangle(&mut stage, 1.0, 1.0);
    stage.shift(target, [-3.0, -2.0, 0.0]);
    bubble.pin_to(&mut stage, target, true).unwrap();
    assert_eq!(bubble.direction(), BubbleDirection::Left);
    close(
        bubble.get_tip(&stage).unwrap(),
        stage.get_bounding_box_point(target, UR),
    );
    let copied = bubble.copy(&mut stage).unwrap();
    bubble.clear(&mut stage).unwrap();
    assert_eq!(bubble.content(), None);
    assert!(stage.contains(content));
    assert_ne!(copied.content(), Some(content));
    assert!(copied.get_tip(&stage).is_ok());
}

#[test]
fn refusal_paths_do_not_move_content_or_change_scene_edges() {
    let mut stage = Stage::new();
    let mut bubble = SpeechBubble::new().add_to(&mut stage, None).unwrap();
    let candidate = rectangle(&mut stage, 4.0, 4.0);
    let original_points = points(&stage, candidate);
    let children = stage.get(bubble.mobject()).unwrap().submobjects().to_vec();
    for buff in [f64::NAN, -1.0, 100.0] {
        assert!(
            bubble
                .add_content_with_buff(&mut stage, candidate, buff)
                .is_err()
        );
        assert_eq!(points(&stage, candidate), original_points);
        assert_eq!(stage.get(bubble.mobject()).unwrap().submobjects(), children);
    }
    assert!(matches!(
        bubble.add_content(&mut stage, bubble.body()),
        Err(BubbleError::OverlappingFamily)
    ));
    assert!(matches!(
        bubble.pin_to(&mut stage, bubble.mobject(), true),
        Err(BubbleError::OverlappingFamily)
    ));
    let mut foreign = Stage::new();
    let foreign_content = rectangle(&mut foreign, 1.0, 1.0);
    assert!(matches!(
        bubble.add_content(&mut stage, foreign_content),
        Err(BubbleError::Stage(StageError::StaleHandle))
    ));
    let body_before = points(&stage, bubble.body());
    assert!(
        bubble
            .move_tip_to(&mut stage, [f64::NAN, 0.0, 0.0])
            .is_err()
    );
    assert_eq!(points(&stage, bubble.body()), body_before);
    stage.delete(candidate).unwrap();
    assert!(matches!(
        bubble.add_content(&mut stage, candidate),
        Err(BubbleError::Stage(StageError::StaleHandle))
    ));
    let recycled = rectangle(&mut stage, 1.0, 1.0);
    assert_ne!(candidate, recycled);
    assert!(matches!(
        bubble.add_content(&mut stage, candidate),
        Err(BubbleError::Stage(StageError::StaleHandle))
    ));
}

#[test]
fn changed_body_topology_and_shared_body_content_are_refused_before_mutation() {
    let mut stage = Stage::new();
    let mut bubble = SpeechBubble::new().add_to(&mut stage, None).unwrap();
    let added = rectangle(&mut stage, 0.5, 0.5);
    stage.attach(bubble.body(), added).unwrap();
    let before = points(&stage, bubble.mobject());
    assert!(matches!(
        bubble.resize_to_content(&mut stage, 0.25),
        Err(BubbleError::ChangedTopology)
    ));
    assert_eq!(points(&stage, bubble.mobject()), before);
    stage.detach(bubble.body(), added);
    let content = bubble.content().unwrap();
    stage.attach(content, bubble.body()).unwrap();
    let before = points(&stage, bubble.mobject());
    assert!(matches!(
        bubble.flip(&mut stage, true),
        Err(BubbleError::OverlappingFamily)
    ));
    assert_eq!(points(&stage, bubble.mobject()), before);
}

#[test]
fn invalid_thought_geometry_does_not_consume_rng_or_allocate_a_family() {
    let mut stage = Stage::new();
    let content = rectangle(&mut stage, 2.0, 1.0);
    let mut rng = RngRoot::from_seed(23).substream("bubble").sequential();
    let state = rng.clone();
    let epoch = stage.topology_epoch();
    for radius in [0.0, -0.1, f64::NAN, f64::from_bits(1)] {
        let mut builder = ThoughtBubble::new();
        builder.shape.bulge_radius = radius;
        assert!(builder.add_to(&mut stage, Some(content), &mut rng).is_err());
        assert_eq!(rng, state);
        assert_eq!(stage.topology_epoch(), epoch);
        assert!(stage.get(content).unwrap().parents().is_empty());
    }
    let mut speech = SpeechBubble::new();
    speech.config = BubbleConfig {
        filler_shape: (0.0, 1.0),
        ..speech.config
    };
    assert!(speech.add_to(&mut stage, None).is_err());
    assert_eq!(stage.topology_epoch(), epoch);
}
