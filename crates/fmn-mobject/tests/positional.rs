//! Positional-parity corpus (fm-jru, §8.4, §16.4): every scenario in
//! `fixtures/positional.txt` is rebuilt as a Stage family, driven through the
//! positional API with the same parameters the Reference reproduction used
//! (`scripts/gen_positional_fixtures.py`), and checked point-for-point plus the
//! root bounding box. Loose f32 tolerance, since we compute in f64 over f32
//! records.

use fmn_core::constants::{DL, DOWN, LEFT, ORIGIN, RIGHT, UP, UR};
use fmn_core::types::Vec3;
use fmn_mobject::{Mob, Mobject, Stage};

/// Absolute tolerance: coordinates are O(10) and stored as f32 (~1e-6 relative),
/// with a few chained ops; 2e-3 is comfortably loose per §16.4.
const TOL: f64 = 2e-3;

struct InNode {
    parent: i64,
    points: Vec<Vec3>,
}

struct Scenario {
    name: String,
    nodes: Vec<InNode>,
    out: Vec<Vec<Vec3>>,
    bbox: [Vec3; 3],
}

fn parse_points(fields: &[&str]) -> Vec<Vec3> {
    let (chunks, _rem) = fields.as_chunks::<3>();
    chunks
        .iter()
        .map(|c| {
            [
                c[0].parse().unwrap(),
                c[1].parse().unwrap(),
                c[2].parse().unwrap(),
            ]
        })
        .collect()
}

fn load() -> Vec<Scenario> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/positional.txt");
    let text = std::fs::read_to_string(path).expect("positional fixture present");
    let mut scenarios = Vec::new();
    let mut cur: Option<Scenario> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let tok: Vec<&str> = line.split_whitespace().collect();
        match tok[0] {
            "SCENARIO" => {
                cur = Some(Scenario {
                    name: tok[1].to_string(),
                    nodes: Vec::new(),
                    out: Vec::new(),
                    bbox: [[0.0; 3]; 3],
                });
            }
            "NODES" => {}
            "IN" => {
                let s = cur.as_mut().unwrap();
                let parent: i64 = tok[2].parse().unwrap();
                s.nodes.push(InNode {
                    parent,
                    points: parse_points(&tok[4..]),
                });
            }
            "OUT" => {
                let s = cur.as_mut().unwrap();
                s.out.push(parse_points(&tok[3..]));
            }
            "BBOX" => {
                let s = cur.as_mut().unwrap();
                let v = parse_points(&tok[1..]);
                s.bbox = [v[0], v[1], v[2]];
            }
            "END" => scenarios.push(cur.take().unwrap()),
            other => panic!("unknown fixture token {other}"),
        }
    }
    scenarios
}

/// Build the family in the stage; return the node handles in index order.
fn build(stage: &mut Stage, s: &Scenario) -> Vec<Mob> {
    let handles: Vec<Mob> = s
        .nodes
        .iter()
        .map(|n| stage.add(Mobject::from_points(&n.points)))
        .collect();
    for (i, n) in s.nodes.iter().enumerate() {
        if n.parent >= 0 {
            stage
                .attach(handles[n.parent as usize], handles[i])
                .expect("attach");
        }
    }
    handles
}

/// Apply the operation named by the scenario, with the same parameters the
/// Reference reproduction used.
fn apply(stage: &mut Stage, name: &str, h: &[Mob]) {
    match name {
        "shift" => {
            stage.shift(h[0], [1.0, -2.0, 0.5]);
        }
        "scale_center" => {
            stage.scale(h[0], 2.0);
        }
        "scale_about_edge" => {
            stage.scale_about(h[0], 0.5, None, Some(DL));
        }
        "scale_about_point" => {
            stage.scale_about(h[0], 3.0, Some([1.0, 1.0, 0.0]), None);
        }
        "stretch_x" => {
            stage.stretch(h[0], 2.5, 0);
        }
        "stretch_y" => {
            stage.stretch(h[0], 0.5, 1);
        }
        "center" => {
            stage.center(h[0]);
        }
        "to_edge_up" => {
            stage.to_edge(h[0], UP, 0.5);
        }
        "to_edge_left" => {
            stage.to_edge(h[0], LEFT, 0.5);
        }
        "to_corner_ur" => {
            stage.to_corner(h[0], UR, 0.25);
        }
        "next_to_right" => {
            stage.next_to(h[0], h[1], RIGHT, 0.25, ORIGIN);
        }
        "next_to_up_aligned" => {
            stage.next_to(h[0], h[1], UP, 0.5, LEFT);
        }
        "next_to_point" => {
            stage.next_to(h[0], [2.0, 2.0, 0.0], DOWN, 0.1, ORIGIN);
        }
        "move_to_point" => {
            stage.move_to(h[0], [1.0, 1.0, 0.0], ORIGIN);
        }
        "move_to_mob_edge" => {
            stage.move_to(h[0], h[1], UP);
        }
        "align_to_mob" => {
            stage.align_to(h[0], h[1], UP);
        }
        "set_x" => {
            stage.set_x(h[0], -3.0);
        }
        "set_y" => {
            stage.set_y(h[0], 2.5);
        }
        "set_width_scale" => {
            stage.set_width(h[0], 4.0, false);
        }
        "set_height_stretch" => {
            stage.set_height(h[0], 3.0, true);
        }
        "match_width" => {
            stage.match_width(h[0], h[1]);
        }
        "match_x" => {
            stage.match_x(h[0], h[1]);
        }
        "nested_bbox_shift" => {
            stage.shift(h[0], [1.0, 1.0, 0.0]);
        }
        "arrange_right" => {
            stage.arrange(h[0], RIGHT, 0.5, true);
        }
        "arrange_down" => {
            stage.arrange(h[0], DOWN, 0.25, true);
        }
        "arrange_in_grid" => {
            stage.arrange_in_grid(h[0], 2, 2, 0.3, 0.3, ORIGIN);
        }
        other => panic!("scenario {other} has no op binding in the parity test"),
    }
}

fn own_points(stage: &Stage, mob: Mob) -> Vec<Vec3> {
    stage.get_points(mob).unwrap_or_default()
}

fn close(a: Vec3, b: Vec3, ctx: &str) {
    for k in 0..3 {
        assert!(
            (a[k] - b[k]).abs() <= TOL,
            "{ctx}: axis {k}: got {} want {} (Δ {})",
            a[k],
            b[k],
            (a[k] - b[k]).abs()
        );
    }
}

#[test]
fn positional_parity_corpus() {
    let scenarios = load();
    assert!(scenarios.len() >= 20, "expected the full corpus");
    for s in &scenarios {
        let mut stage = Stage::new();
        let handles = build(&mut stage, s);
        apply(&mut stage, &s.name, &handles);

        // Every node's own points must match the Reference reproduction.
        for (i, &mob) in handles.iter().enumerate() {
            let got = own_points(&stage, mob);
            let want = &s.out[i];
            assert_eq!(got.len(), want.len(), "{}: node {i} point count", s.name);
            for (g, w) in got.iter().zip(want) {
                close(*g, *w, &format!("{} node {i}", s.name));
            }
        }

        // The root bounding box must match [min, mid, max].
        let bb = stage.get_bounding_box(handles[0]);
        close(bb.min, s.bbox[0], &format!("{} bbox.min", s.name));
        close(bb.mid, s.bbox[1], &format!("{} bbox.mid", s.name));
        close(bb.max, s.bbox[2], &format!("{} bbox.max", s.name));
    }
}

#[test]
fn affine_motion_has_an_independent_revision_and_object_space_buffer() {
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_points(&[
        [0.0, 0.0, 0.0],
        [1.0, 0.5, 0.0],
        [2.0, 1.0, 0.0],
    ]));
    let object_points = stage.get_object_points(mob).unwrap();
    let point_revision = stage
        .get(mob)
        .unwrap()
        .buffer
        .field_revision("point")
        .unwrap();
    let placement_revision = stage.placement_revision(mob).unwrap();

    stage.shift(mob, [5.0, -2.0, 0.25]);
    assert_eq!(stage.get_object_points(mob).unwrap(), object_points);
    assert_eq!(
        stage.get(mob).unwrap().buffer.field_revision("point"),
        Some(point_revision)
    );
    assert!(stage.placement_revision(mob).unwrap() > placement_revision);
    assert_eq!(
        stage.get_points(mob).unwrap(),
        vec![[5.0, -2.0, 0.25], [6.0, -1.5, 0.25], [7.0, -1.0, 0.25],]
    );

    stage.rotate(
        mob,
        std::f64::consts::FRAC_PI_2,
        [0.0, 0.0, 1.0],
        Some([5.0, -2.0, 0.25]),
        None,
    );
    stage.scale_about(mob, 2.0, Some([5.0, -2.0, 0.25]), None);
    assert_eq!(
        stage.get_object_points(mob).unwrap(),
        object_points,
        "rotation and scale are placement edits too"
    );
    assert_eq!(
        stage.get(mob).unwrap().buffer.field_revision("point"),
        Some(point_revision)
    );
}

#[test]
fn affine_motion_stays_visible_through_an_existing_zero_copy_view() {
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_points(&[[1.0, 2.0, 0.0], [3.0, 4.0, 0.0]]));
    let view = stage.get_mut(mob).expect("live").buffer.export_view(false);

    stage.shift(mob, [5.0, -1.0, 0.25]);

    assert_eq!(view.read(0, "point"), Some(vec![6.0, 1.0, 0.25]));
    assert_eq!(view.read(1, "point"), Some(vec![8.0, 3.0, 0.25]));
    assert!(
        stage.placement(mob).unwrap().is_identity(),
        "a pinned authoritative buffer receives the affine write directly"
    );
}

#[test]
fn an_arbitrary_point_map_bakes_the_current_placement_once() {
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_points(&[[1.0, 2.0, 0.0]]));
    stage.shift(mob, [3.0, 4.0, 0.0]);
    let before = stage
        .get(mob)
        .unwrap()
        .buffer
        .field_revision("point")
        .unwrap();

    stage.apply_points_function(
        mob,
        |point| [point[0] * 2.0, point[1] - 1.0, point[2]],
        None,
        None,
    );

    assert!(stage.placement(mob).unwrap().is_identity());
    assert_eq!(stage.get_points(mob).unwrap(), vec![[8.0, 5.0, 0.0]]);
    assert!(
        stage
            .get(mob)
            .unwrap()
            .buffer
            .field_revision("point")
            .unwrap()
            > before
    );
}

#[test]
fn copies_and_in_memory_snapshots_carry_placement_state() {
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_points(&[[1.0, 2.0, 0.0]]));
    stage.shift(mob, [3.0, -1.0, 0.5]);
    stage.stretch_about(mob, 2.0, 0, Some([4.0, 1.0, 0.5]), None);
    let placement = stage.placement(mob).unwrap();
    let world_points = stage.get_points(mob).unwrap();

    let copy = stage.copy_family(mob).unwrap();
    assert!(stage.placement(copy).unwrap().same_bits(placement));
    assert_eq!(stage.get_points(copy).unwrap(), world_points);

    let snapshot = stage.snapshot();
    stage.shift(mob, [100.0, 0.0, 0.0]);
    stage.restore(&snapshot);
    assert!(stage.placement(mob).unwrap().same_bits(placement));
    assert_eq!(stage.get_points(mob).unwrap(), world_points);
}

#[test]
fn shift_onto_screen_pulls_back_only_the_edges_beyond_the_inset_frame() {
    use fmn_core::constants::{FRAME_X_RADIUS, FRAME_Y_RADIUS};
    let square = |center: Vec3| {
        let [x, y, z] = center;
        Mobject::from_points(&[
            [x - 0.5, y - 0.5, z],
            [x + 0.5, y - 0.5, z],
            [x + 0.5, y + 0.5, z],
            [x - 0.5, y + 0.5, z],
        ])
    };
    let mut stage = Stage::new();
    // Beyond the right and top edges: both pulled to `radius - buff`.
    let off = stage.add(square([9.0, 5.0, 0.0]));
    stage.shift_onto_screen(off, 0.5);
    assert!((stage.get_right(off)[0] - (FRAME_X_RADIUS - 0.5)).abs() < TOL);
    assert!((stage.get_top(off)[1] - (FRAME_Y_RADIUS - 0.5)).abs() < TOL);
    // Inside the inset frame: untouched.
    let inside = stage.add(square([1.0, -1.0, 0.0]));
    stage.shift_onto_screen(inside, 0.5);
    assert!((stage.get_center(inside)[0] - 1.0).abs() < TOL);
    assert!((stage.get_center(inside)[1] + 1.0).abs() < TOL);
    // Beyond the bottom-left: pulled to the inset bottom-left.
    let low = stage.add(square([-9.0, -6.0, 0.0]));
    stage.shift_onto_screen(low, 0.25);
    assert!((stage.get_left(low)[0] + (FRAME_X_RADIUS - 0.25)).abs() < TOL);
    assert!((stage.get_bottom(low)[1] + (FRAME_Y_RADIUS - 0.25)).abs() < TOL);
}

fn layout_box(min: Vec3, max: Vec3) -> Mobject {
    Mobject::from_points(&[min, max])
}

fn near(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
}

#[test]
fn fit_layout_preserves_nested_child_sizes_order_and_the_selected_edge() {
    let mut stage = Stage::new();
    let group = stage.add(Mobject::group(vec![
        layout_box([0.0, -1.0, 0.0], [1.0, 1.0, 1.0]),
        Mobject::group(vec![layout_box([2.0, -2.0, 0.0], [4.0, 2.0, 1.0])]),
        layout_box([5.0, 0.0, 0.0], [6.0, 1.0, 1.0]),
    ]));
    stage.shift(group, [4.0, 3.0, 2.0]);
    let children = stage.get(group).unwrap().submobjects().to_vec();
    let edge_before = stage.get_bounding_box_point(group, LEFT);
    let centers_before: Vec<Vec3> = children.iter().map(|&m| stage.get_center(m)).collect();
    let point_columns: Vec<_> = stage
        .family(group)
        .into_iter()
        .map(|m| {
            (
                m,
                stage.get_object_points(m),
                stage.get(m).unwrap().buffer.field_revision("point"),
            )
        })
        .collect();

    stage.arrange_to_fit_width(group, 8.0, LEFT);

    near(stage.get_width(group), 8.0);
    assert_eq!(stage.get_bounding_box_point(group, LEFT), edge_before);
    assert_eq!(stage.get(group).unwrap().submobjects(), children);
    for ((&child, width), before) in children.iter().zip([1.0, 2.0, 1.0]).zip(centers_before) {
        near(stage.get_width(child), width);
        near(stage.get_y(child), before[1]);
        near(stage.get_z(child), before[2]);
    }
    for pair in children.windows(2) {
        near(
            stage.get_left(pair[1])[0] - stage.get_right(pair[0])[0],
            2.0,
        );
    }
    for (member, points, revision) in point_columns {
        assert_eq!(
            stage.get_object_points(member),
            points,
            "layout keeps retained geometry"
        );
        assert_eq!(
            stage.get(member).unwrap().buffer.field_revision("point"),
            revision
        );
    }

    // The Reference permits overlap: there is no hidden clamp to zero gap.
    stage.arrange_to_fit_width(group, 2.0, LEFT);
    near(stage.get_width(group), 2.0);
    for pair in children.windows(2) {
        near(
            stage.get_left(pair[1])[0] - stage.get_right(pair[0])[0],
            -1.0,
        );
    }
    assert_eq!(stage.get_bounding_box_point(group, LEFT), edge_before);
}

#[test]
fn fit_layout_supports_height_depth_and_point_free_or_single_child_groups() {
    let mut stage = Stage::new();
    let group = stage.add(Mobject::group(vec![
        layout_box([-2.0, -0.5, -0.5], [0.0, 0.5, 0.5]),
        layout_box([1.0, 1.5, 1.5], [3.0, 2.5, 2.5]),
    ]));
    let center = stage.get_center(group);
    stage.arrange_to_fit_height(group, 6.0, ORIGIN);
    near(stage.get_height(group), 6.0);
    stage.arrange_to_fit_depth(group, 7.0, ORIGIN);
    near(stage.get_depth(group), 7.0);
    assert_eq!(stage.get_center(group), center);
    for &child in stage.get(group).unwrap().submobjects() {
        near(stage.get_width(child), 2.0);
        near(stage.get_height(child), 1.0);
        near(stage.get_depth(child), 1.0);
    }

    for detached in [Mobject::new(), Mobject::group(vec![Mobject::new()])] {
        let empty = stage.add(detached);
        stage.shift(empty, [3.0, -2.0, 1.0]);
        let before: Vec<_> = stage
            .family(empty)
            .into_iter()
            .map(|m| (m, stage.placement(m)))
            .collect();
        stage.arrange_to_fit_width(empty, 10.0, RIGHT);
        for (member, placement) in before {
            assert_eq!(
                stage.placement(member),
                placement,
                "trivial child counts are unchanged"
            );
        }
    }

    let empty_children = stage.add(Mobject::group(vec![Mobject::new(), Mobject::new()]));
    stage.arrange_to_fit_width(empty_children, 4.0, ORIGIN);
    for member in stage.family(empty_children) {
        assert!(stage.get_center(member).into_iter().all(f64::is_finite));
        assert!(stage.get(member).unwrap().buffer.is_empty());
    }
}

#[test]
fn spacing_preserves_nested_child_sizes_with_an_explicit_parent_pivot() {
    let mut stage = Stage::new();
    let group = stage.add(Mobject::group(vec![
        layout_box([-2.5, -0.5, 0.0], [-1.5, 0.5, 0.0]),
        Mobject::group(vec![
            layout_box([2.0, -1.0, 0.0], [2.5, 1.0, 0.0]),
            layout_box([3.5, -1.0, 0.0], [4.0, 1.0, 0.0]),
        ]),
    ]));
    let children = stage.get(group).unwrap().submobjects().to_vec();
    let before: Vec<_> = children
        .iter()
        .map(|&m| (stage.get_center(m), stage.get_width(m), stage.get_height(m)))
        .collect();
    let pivot = [1.0, 2.0, 0.0];
    stage.space_out_submobjects_about(group, 2.0, Some(pivot), None);
    for (&child, (center, width, height)) in children.iter().zip(before) {
        let actual = stage.get_center(child);
        for dim in 0..3 {
            near(actual[dim], pivot[dim] + 2.0 * (center[dim] - pivot[dim]));
        }
        near(stage.get_width(child), width);
        near(stage.get_height(child), height);
    }
    let nested = stage.get(children[1]).unwrap().submobjects();
    near(
        stage.get_left(nested[1])[0] - stage.get_right(nested[0])[0],
        1.0,
    );

    // Only direct children scale back. A parent's own point run stays scaled.
    let parent = stage.add(layout_box([-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]));
    let child = stage.add(layout_box([-0.5, -0.5, 0.0], [0.5, 0.5, 0.0]));
    stage.attach(parent, child).unwrap();
    stage.space_out_submobjects(parent, 2.0);
    assert_eq!(
        stage.get_points(parent).unwrap(),
        vec![[-2.0, 0.0, 0.0], [2.0, 0.0, 0.0]]
    );
    near(stage.get_width(child), 1.0);
}

#[test]
fn conditional_size_constraints_preserve_noops_and_choose_stretch_or_uniform_scale() {
    let mut stage = Stage::new();
    let mob = stage.add(layout_box([-2.0, -1.0, -3.0], [2.0, 1.0, 3.0]));
    stage.shift(mob, [1.0, 2.0, 3.0]);
    let before = stage.placement_revision(mob);
    stage.set_max_width(mob, 8.0, false);
    stage.set_min_height(mob, 1.0, false);
    assert_eq!(
        stage.placement_revision(mob),
        before,
        "satisfied constraints do not invalidate caches"
    );

    stage.set_max_width(mob, 2.0, false);
    near(stage.get_height(mob), 1.0);
    near(stage.get_depth(mob), 3.0);
    stage.set_min_height(mob, 4.0, true);
    stage.set_max_depth(mob, 2.0, true);
    stage.set_min_width(mob, 4.0, true);
    stage.set_max_height(mob, 3.0, true);
    stage.set_min_depth(mob, 5.0, true);
    near(stage.get_width(mob), 4.0);
    near(stage.get_height(mob), 3.0);
    near(stage.get_depth(mob), 5.0);
    stage.set_shape(mob, Some(2.0), None, Some(3.0));
    near(stage.get_width(mob), 2.0);
    near(stage.get_height(mob), 3.0);
    near(stage.get_depth(mob), 3.0);
    assert_eq!(stage.get_center(mob), [1.0, 2.0, 3.0]);

    let empty = stage.add(Mobject::new());
    stage.set_min_width(empty, 2.0, false);
    stage.set_shape(empty, Some(1.0), Some(1.0), Some(1.0));
    assert_eq!(stage.get_center(empty), ORIGIN);
    near(stage.get_width(empty), 0.0);
}

#[test]
fn replace_and_surround_keep_identity_geometry_and_reference_buffer_semantics() {
    let mut stage = Stage::new();
    let source = stage.add(layout_box([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]));
    let target = stage.add(layout_box([1.0, -3.0, 0.0], [7.0, -1.0, 1.0]));
    let points = stage.get_object_points(source);
    let target_points = stage.get_points(target);
    let count = stage.len();
    stage.replace(source, target, 0, false);
    near(stage.get_width(source), 6.0);
    near(stage.get_height(source), 6.0);
    assert_eq!(stage.get_center(source), stage.get_center(target));
    stage.replace(source, target, 0, true);
    assert_eq!(
        stage.get_bounding_box(source),
        stage.get_bounding_box(target)
    );
    stage.surround(source, target, 0, true, 0.6);
    near(stage.get_width(source), 6.6);
    near(stage.get_height(source), 2.2);
    near(stage.get_depth(source), 1.1);
    assert_eq!(stage.get_object_points(source), points);
    assert_eq!(stage.get_points(target), target_points);
    assert_eq!(
        stage.len(),
        count,
        "replace is a fit, not scene or family replacement"
    );

    let empty = stage.add(Mobject::new());
    stage.shift(empty, [-10.0, 0.0, 0.0]);
    let center = stage.get_center(source);
    let width = stage.get_width(source);
    stage.replace(source, empty, 0, false);
    assert_eq!(
        stage.get_center(source),
        center,
        "an empty leaf target shrinks in place"
    );
    assert!(stage.get_width(source) > 0.0);
    assert!((stage.get_width(source) / width - 1e-8).abs() < 1e-15);
}
