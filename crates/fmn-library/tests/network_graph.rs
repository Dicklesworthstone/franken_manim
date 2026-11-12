//! fm-n64 tranche 1: the enhanced-tier `NetworkGraph` — determinism
//! digests per layout kernel, analytic ground truth for the circular
//! rule, partition/BFS refusals, and one bit-locked self-golden.

use std::sync::Arc;

use fmn_anim::{AnimConfig, AnimError, Animation, RateFunc};
use fmn_core::color::Srgb;
use fmn_core::types::Vec3;
use fmn_hash::sha256;
use fmn_mobject::{Mob, Stage, StageError};
use fnx_classes::Graph;

use fmn_library::network_graph::{
    DEFAULT_SPRING_IDEAL_EDGE, FlowAlongEdges, GraphAnimationError, GraphFlowConfig, GraphLayout,
    GraphTraversal, GraphTraversalConfig, LayoutTransition, NetworkGraph, NetworkGraphMobject,
    TraverseGraph,
};

/// A 5-cycle plus a chord — small enough to inspect, rich enough to have
/// two BFS levels and a non-trivial edge set.
fn sample_graph() -> Graph {
    let mut graph = Graph::strict();
    for label in ["a", "b", "c", "d", "e"] {
        graph.add_node(label);
    }
    for (u, v) in [
        ("a", "b"),
        ("b", "c"),
        ("c", "d"),
        ("d", "e"),
        ("e", "a"),
        ("b", "d"),
    ] {
        graph
            .add_edge(u, v)
            .unwrap_or_else(|error| fail(format!("edge {u}-{v}: {error}")));
    }
    graph
}

/// Fail with a message. UBS bans literal `panic!`/`unreachable!`; test
/// refusals go through [`std::panic::panic_any`] like the svg suite's.
fn fail(message: String) -> ! {
    std::panic::panic_any(message)
}

fn position_digest(network: &NetworkGraph) -> String {
    // Depth-first over the deterministic node order: label, x, y.
    let mut bytes = Vec::new();
    for label in network.nodes() {
        let point = network
            .position(label)
            .unwrap_or_else(|| fail(format!("unpositioned {label}")));
        bytes.extend_from_slice(label.as_bytes());
        bytes.extend_from_slice(&point[0].to_bits().to_le_bytes());
        bytes.extend_from_slice(&point[1].to_bits().to_le_bytes());
    }
    sha256(&bytes).to_hex()
}

#[test]
fn circular_positions_hit_the_analytic_circle() {
    let network = NetworkGraph::new(sample_graph())
        .laid_out(&GraphLayout::Circular)
        .unwrap_or_else(|error| fail(format!("{error}")));
    let n = 5.0;
    for (index, label) in ["a", "b", "c", "d", "e"].into_iter().enumerate() {
        let theta = index as f64 * std::f64::consts::TAU / n;
        let point = network.position(label).expect("laid out");
        assert!((point[0] - theta.cos()).abs() < 1e-12, "{label} x");
        assert!((point[1] - theta.sin()).abs() < 1e-12, "{label} y");
    }
}

#[test]
fn shell_partition_is_enforced_by_name() {
    let error = NetworkGraph::new(sample_graph())
        .laid_out(&GraphLayout::Shell {
            shells: vec![
                vec!["a".into(), "b".into()],
                vec!["b".into(), "c".into(), "d".into(), "e".into()],
            ],
        })
        .unwrap_err();
    match error {
        fmn_library::network_graph::NetworkGraphError::ShellPartitionMismatch {
            duplicated,
            missing,
        } => {
            assert_eq!(duplicated, vec!["b".to_owned()]);
            // 'a' appears only in an inner slot? No — it is present; nothing
            // is missing in this partition.
            assert!(missing.is_empty(), "{missing:?}");
        }
        other => fail(format!("expected partition mismatch, got {other:?}")),
    }

    let error = NetworkGraph::new(sample_graph())
        .laid_out(&GraphLayout::Shell {
            shells: vec![vec!["a".into(), "zzz".into()]],
        })
        .unwrap_err();
    assert!(
        matches!(
            error,
            fmn_library::network_graph::NetworkGraphError::UnknownNode(ref node) if node == "zzz"
        ),
        "{error:?}"
    );
}

#[test]
fn breadth_first_levels_expand_in_sorted_order() {
    let network = NetworkGraph::new(sample_graph())
        .laid_out(&GraphLayout::BreadthFirst { root: "a".into() })
        .unwrap_or_else(|error| fail(format!("{error}")));
    let center = |label: &str| {
        let p = network.position(label).expect("laid out");
        (p[0] * p[0] + p[1] * p[1]).sqrt()
    };
    let _radius_of = |label: &str| network.position(label).map(|p| p[0]);
    // Level 0 sits at the origin ring (radius 0).
    assert!(center("a") < 1e-12);
    // Levels 1 and 2 sit on their rings; sorted expansion puts b before e
    // on level 1's arc (angle order matches sorted labels).
    assert!(center("b") > center("a"));
    assert!(center("c") > center("b"));
}

#[test]
fn spring_is_bit_reproducible_for_a_fixed_seed() {
    let layout = GraphLayout::Spring {
        seed: 0x5f3759df,
        iterations: 30,
        ideal_edge: DEFAULT_SPRING_IDEAL_EDGE,
    };
    let first = position_digest(
        &NetworkGraph::new(sample_graph())
            .laid_out(&layout)
            .unwrap_or_else(|error| fail(format!("{error}"))),
    );
    let second = position_digest(
        &NetworkGraph::new(sample_graph())
            .laid_out(&layout)
            .unwrap_or_else(|error| fail(format!("{error}"))),
    );
    assert_eq!(first, second, "same seed must replay bit-identically");

    let other = position_digest(
        &NetworkGraph::new(sample_graph())
            .laid_out(&GraphLayout::Spring {
                seed: 0x5f3759de,
                iterations: 30,
                ideal_edge: DEFAULT_SPRING_IDEAL_EDGE,
            })
            .unwrap_or_else(|error| fail(format!("{error}"))),
    );
    assert_ne!(first, other, "different seeds must diverge");
}

#[test]
fn built_family_paints_edges_under_vertices() {
    let network = NetworkGraph::new(sample_graph())
        .laid_out(&GraphLayout::Circular)
        .unwrap_or_else(|error| fail(format!("{error}")));
    let family = network
        .build()
        .unwrap_or_else(|error| fail(format!("{error}")));
    // Six edges then five dots: [edges…, vertices…].
    assert_eq!(family.children().len(), 6 + 5);
}

#[test]
fn self_loop_is_a_visible_closed_circle_tangent_to_its_vertex() {
    let network = NetworkGraph::from_edge_list(&["a"], &[("a", "a")])
        .laid_out(&GraphLayout::BreadthFirst { root: "a".into() })
        .unwrap();
    let position = network.position("a").unwrap();
    let relative = |point: Vec3| std::array::from_fn(|axis| point[axis] + position[axis]);
    let family = network.build().unwrap();
    assert_eq!(family.children().len(), 2, "one edge, then one vertex");
    let self_loop = &family.children()[0];
    let vertex = &family.children()[1];

    let (min, max) = self_loop.extent().unwrap();
    assert_point_close(min, relative([-0.27, 0.09, 0.0]));
    assert_point_close(max, relative([0.27, 0.63, 0.0]));
    let (_, vertex_max) = vertex.extent().unwrap();
    assert!((min[1] - vertex_max[1]).abs() < 1e-12);

    let path = self_loop.path().unwrap();
    assert_eq!(path.subpaths().len(), 1);
    assert!(path.is_closed());
    let first = self_loop.points()[0];
    assert_point_close(first, relative([0.0, 0.09, 0.0]));
    assert_point_close(*self_loop.points().last().unwrap(), first);
    let handle = self_loop.points()[1];
    assert!(
        handle[0] > first[0],
        "the pulse leaves the node to the right"
    );
    assert!((handle[1] - first[1]).abs() < 1e-12, "horizontal tangent");
    for anchor in self_loop.points().iter().step_by(2) {
        let radius_squared =
            (anchor[0] - position[0]).powi(2) + (anchor[1] - position[1] - 0.36).powi(2);
        assert!((radius_squared - 0.27 * 0.27).abs() < 1e-12);
        assert_eq!(anchor[2], position[2]);
    }

    assert_eq!(self_loop.style().stroke_color, fmn_core::constants::GREY_B);
    assert_eq!(self_loop.style().stroke_width, 2.5);
    assert_eq!(self_loop.style().stroke_opacity, 1.0);
    assert_eq!(self_loop.style().fill_opacity, 0.0);
    assert_eq!(vertex.style().fill_color, fmn_core::constants::BLUE_D);
    assert_eq!(vertex.style().fill_opacity, 1.0);
}

#[test]
fn self_loops_keep_edge_identity_and_deterministic_geometry_in_every_layout() {
    let nodes = ["z", "a", "m"];
    let edges = [("a", "a"), ("z", "a"), ("m", "m"), ("a", "m")];
    for layout in [
        GraphLayout::Circular,
        GraphLayout::Shell {
            shells: vec![vec!["m".into(), "z".into()], vec!["a".into()]],
        },
        GraphLayout::BreadthFirst { root: "z".into() },
        GraphLayout::Spring {
            seed: 0x5f3759df,
            iterations: 30,
            ideal_edge: DEFAULT_SPRING_IDEAL_EDGE,
        },
    ] {
        let build = || {
            NetworkGraph::from_edge_list(&nodes, &edges)
                .laid_out(&layout)
                .unwrap()
        };
        let network = build();
        let family = network.build().unwrap();
        assert_eq!(family, build().build().unwrap(), "{layout:?}");

        let ordered_edges = network.graph().edges_ordered_borrowed();
        let edge_count = ordered_edges.len();
        assert_eq!(edge_count, edges.len());
        assert_eq!(family.children().len(), edge_count + nodes.len());
        for ((from, to, _), edge) in ordered_edges.into_iter().zip(family.children()) {
            let position = network.position(from).unwrap();
            if from == to {
                let contact = [position[0], position[1] + 0.09, position[2]];
                assert_point_close(edge.points()[0], contact);
                assert_point_close(*edge.points().last().unwrap(), contact);
                let (min, max) = edge.extent().unwrap();
                assert!(max[0] - min[0] > 0.5 && max[1] - min[1] > 0.5);
                assert!(edge.path().unwrap().is_closed());
            } else {
                assert_point_close(edge.points()[0], position);
                assert_point_close(
                    *edge.points().last().unwrap(),
                    network.position(to).unwrap(),
                );
            }
        }
        for (label, vertex) in network.nodes().iter().zip(&family.children()[edge_count..]) {
            let (min, max) = vertex.extent().unwrap();
            assert_point_close(
                std::array::from_fn(|axis| (min[axis] + max[axis]) / 2.0),
                network.position(label).unwrap(),
            );
            assert_eq!(vertex.style().fill_color, fmn_core::constants::BLUE_D);
            assert_eq!(vertex.style().fill_opacity, 1.0);
        }
    }
}

#[test]
fn self_golden_locks_the_canonical_build() {
    let mut star = Graph::strict();
    for label in ["hub", "n1", "n2", "n3", "n4"] {
        star.add_node(label);
    }
    for leaf in ["n1", "n2", "n3", "n4"] {
        star.add_edge("hub", leaf)
            .unwrap_or_else(|error| fail(format!("spoke: {error}")));
    }
    let network = NetworkGraph::new(star)
        .laid_out(&GraphLayout::BreadthFirst { root: "hub".into() })
        .unwrap_or_else(|error| fail(format!("{error}")));
    let family = network
        .build()
        .unwrap_or_else(|error| fail(format!("{error}")));
    let mut bytes = Vec::new();
    let mut stack = vec![&family];
    while let Some(current) = stack.pop() {
        for point in current.points() {
            bytes.extend_from_slice(&point[0].to_bits().to_le_bytes());
            bytes.extend_from_slice(&point[1].to_bits().to_le_bytes());
            bytes.extend_from_slice(&point[2].to_bits().to_le_bytes());
        }
        for child in current.children() {
            stack.push(child);
        }
    }
    assert_eq!(
        sha256(&bytes).to_hex(),
        "7d014dc243ce2e1362f41037514bdb7aabc2870f266e45b94da39aeb018e8282",
        "the canonical star build drifted"
    );
}

#[test]
fn layout_transition_animates_between_circular_and_spring() {
    let network = NetworkGraph::new(sample_graph())
        .laid_out(&GraphLayout::Circular)
        .unwrap();
    let layout = GraphLayout::Spring {
        seed: 0x12345678,
        iterations: 20,
        ideal_edge: DEFAULT_SPRING_IDEAL_EDGE,
    };
    let target = network.clone().laid_out(&layout).unwrap();
    let mut stage = Stage::new();
    let graph = network.add_to(&mut stage).unwrap();
    stage.add_to_scene(graph.mobject()).unwrap();
    let mut transition = LayoutTransition::new(&stage, &graph, &layout)
        .unwrap()
        .with_config(AnimConfig {
            rate_func: RateFunc::linear(),
            suspend_mobject_updating: true,
            ..AnimConfig::default()
        });

    // Edits after construction must be the transition's actual begin state.
    stage.scale(graph.mobject(), 1.7);
    stage.shift(graph.mobject(), [2.0, -1.0, 0.25]);
    stage.set_color_lanes(
        graph.node("b").unwrap(),
        "fill_rgba",
        Some([0.2, 0.4, 0.7]),
        Some(0.6),
        false,
    );
    stage.set_scalar_lanes(graph.edge("a", "b").unwrap(), "stroke_width", 4.5, false);
    let profiled_edge = graph.edge("b", "c").unwrap();
    let points = stage.get_points(profiled_edge).unwrap();
    let subdivided: Vec<_> = (0..5)
        .map(|index| {
            let alpha = index as f64 / 4.0;
            std::array::from_fn(|axis| (1.0 - alpha) * points[0][axis] + alpha * points[2][axis])
        })
        .collect();
    stage.set_points(profiled_edge, &subdivided).unwrap();
    let width_profile = vec![1.0, 2.0, 4.5, 3.0, 1.0];
    stage
        .get_mut(profiled_edge)
        .unwrap()
        .buffer
        .write_range("stroke_width", 0, &width_profile);
    let originals: Vec<_> = graph
        .nodes()
        .iter()
        .map(|label| {
            let node = graph.node(label).unwrap();
            (
                node,
                stage.get_center(node),
                stage.get_bounding_box(node).width(),
                rgba(&stage, node, "fill_rgba"),
            )
        })
        .collect();
    let handles = stage.get(graph.mobject()).unwrap().submobjects().to_vec();
    transition.begin(&mut stage).unwrap();
    assert!(transition.freeze_interpolation().is_some());
    // Revisit earlier alphas too: layout interpolation must be absolute.
    for alpha in [0.0, 0.25, 0.75, 0.5, 1.0] {
        transition.interpolate(&mut stage, alpha);
        for (label, &(node, start, width, ref fill)) in graph.nodes().iter().zip(&originals) {
            let destination = target.position(label).unwrap();
            assert_point_close(
                stage.get_center(node),
                std::array::from_fn(|axis| (1.0 - alpha) * start[axis] + alpha * destination[axis]),
            );
            assert!((stage.get_bounding_box(node).width() - width).abs() < 2e-6);
            assert_eq!(&rgba(&stage, node, "fill_rgba"), fill);
        }
        assert_edges_connected(&stage, &network, &graph);
        assert_eq!(stage.get(graph.mobject()).unwrap().submobjects(), &handles);
        assert_eq!(
            stage
                .get(graph.edge("a", "b").unwrap())
                .unwrap()
                .buffer
                .read(0, "stroke_width"),
            Some(vec![4.5])
        );
        assert_eq!(
            stage
                .get(profiled_edge)
                .unwrap()
                .buffer
                .read_column("stroke_width"),
            Some(width_profile.clone())
        );
    }
    transition.finish(&mut stage);
    transition.clean_up_from_scene(&mut stage);
    assert_eq!(stage.roots(), &[graph.mobject()]);
    assert!(!stage.is_animating(graph.mobject()));
    assert!(!stage.is_updating_suspended(graph.mobject()));
}

fn rgba(stage: &Stage, mob: Mob, field: &str) -> Vec<f32> {
    stage.get(mob).unwrap().buffer.read(0, field).unwrap()
}

fn assert_point_close(actual: Vec3, expected: Vec3) {
    for axis in 0..3 {
        assert!(
            (actual[axis] - expected[axis]).abs() < 2e-6,
            "actual={actual:?}, expected={expected:?}"
        );
    }
}

fn assert_edges_connected(stage: &Stage, network: &NetworkGraph, graph: &NetworkGraphMobject) {
    for (from, to, _) in network.graph().edges_ordered_borrowed() {
        let points = stage.get_points(graph.edge(from, to).unwrap()).unwrap();
        assert_point_close(points[0], stage.get_center(graph.node(from).unwrap()));
        assert_point_close(
            *points.last().unwrap(),
            stage.get_center(graph.node(to).unwrap()),
        );
    }
}

fn family_bytes(stage: &Stage, root: Mob) -> Vec<u8> {
    let mut bytes = Vec::new();
    for mob in stage.family(root) {
        let entry = stage.get(mob).unwrap();
        for value in entry.placement().coefficients() {
            bytes.extend_from_slice(&value.to_bits().to_le_bytes());
        }
        let uniforms = entry.uniforms();
        for value in [
            uniforms.is_fixed_in_frame,
            uniforms.anti_alias_width,
            uniforms.joint_type.to_code(),
        ]
        .into_iter()
        .chain(uniforms.shading)
        .chain(uniforms.clip_planes.into_iter().flatten())
        {
            bytes.extend_from_slice(&value.to_bits().to_le_bytes());
        }
        bytes.extend(
            [
                uniforms.flat_stroke,
                uniforms.scale_stroke_with_zoom,
                uniforms.stroke_behind,
                uniforms.depth_test,
                uniforms.use_winding_fill,
            ]
            .map(u8::from),
        );
        for field in entry.buffer.schema().fields() {
            bytes.extend_from_slice(field.name.as_bytes());
            for value in entry.buffer.read_column(&field.name).unwrap() {
                bytes.extend_from_slice(&value.to_bits().to_le_bytes());
            }
        }
    }
    bytes
}

fn rooted_bytes(stage: &Stage) -> Vec<(Mob, Vec<u8>)> {
    stage
        .roots()
        .iter()
        .map(|&root| (root, family_bytes(stage, root)))
        .collect()
}

#[test]
fn layout_transition_preserves_authored_self_loops_through_seeks_and_snapshot_replay() {
    let network = NetworkGraph::from_edge_list(&["a", "b"], &[("a", "a"), ("a", "b")])
        .laid_out(&GraphLayout::Circular)
        .unwrap();
    let layout = GraphLayout::BreadthFirst { root: "b".into() };
    let destination = network.clone().laid_out(&layout).unwrap();
    let mut stage = Stage::new();
    let graph = network.add_to(&mut stage).unwrap();
    stage.add_to_scene(graph.mobject()).unwrap();
    let node = graph.node("a").unwrap();
    let self_loop = graph.edge("a", "a").unwrap();
    let mut transition = LayoutTransition::new(&stage, &graph, &layout)
        .unwrap()
        .with_config(AnimConfig {
            rate_func: RateFunc::linear(),
            ..AnimConfig::default()
        });

    // Author an asymmetric loop after constructing the animation. Its actual
    // begin-time shape, record profile, and placement must survive the move.
    let center = stage.get_center(node);
    let authored: Vec<Vec3> = [
        [0.0, 0.09, 0.0],
        [0.45, 0.09, 0.0],
        [0.45, 0.45, 0.0],
        [0.45, 0.9, 0.0],
        [0.0, 0.9, 0.0],
        [-0.2, 0.9, 0.0],
        [-0.2, 0.45, 0.0],
        [-0.2, 0.09, 0.0],
        [0.0, 0.09, 0.0],
    ]
    .into_iter()
    .map(|point| std::array::from_fn(|axis| point[axis] + center[axis]))
    .collect();
    stage.set_points(self_loop, &authored).unwrap();
    stage.set_color_lanes(
        self_loop,
        "stroke_rgba",
        Some([0.2, 0.6, 0.9]),
        Some(0.75),
        false,
    );
    let widths = vec![1.0, 2.0, 3.0, 4.0, 5.0, 4.0, 3.0, 2.0, 1.0];
    stage
        .get_mut(self_loop)
        .unwrap()
        .buffer
        .write_range("stroke_width", 0, &widths);
    stage.scale(graph.mobject(), 1.7);
    stage.shift(graph.mobject(), [2.0, -1.0, 0.25]);

    let start_center = stage.get_center(node);
    let target_center = destination.position("a").unwrap();
    let original_points = stage.get_points(self_loop).unwrap();
    let colors = stage
        .get(self_loop)
        .unwrap()
        .buffer
        .read_column("stroke_rgba")
        .unwrap();
    let handles = stage.get(graph.mobject()).unwrap().submobjects().to_vec();
    let before = stage.snapshot();
    transition.begin(&mut stage).unwrap();
    assert!(transition.freeze_interpolation().is_some());
    let mut midpoint = None;
    for alpha in [0.0, 0.25, 0.5, 0.75, 1.0, 0.5, 0.0] {
        transition.interpolate(&mut stage, alpha);
        let expected_center: Vec3 = std::array::from_fn(|axis| {
            (1.0 - alpha) * start_center[axis] + alpha * target_center[axis]
        });
        assert_point_close(stage.get_center(node), expected_center);
        let points = stage.get_points(self_loop).unwrap();
        assert_eq!(points.len(), original_points.len());
        for (point, original) in points.iter().zip(&original_points) {
            assert_point_close(
                *point,
                std::array::from_fn(|axis| {
                    original[axis] + expected_center[axis] - start_center[axis]
                }),
            );
        }
        let buffer = &stage.get(self_loop).unwrap().buffer;
        assert_eq!(buffer.read_column("stroke_rgba").as_ref(), Some(&colors));
        assert_eq!(buffer.read_column("stroke_width").as_ref(), Some(&widths));
        assert_eq!(stage.get(graph.mobject()).unwrap().submobjects(), &handles);
        let ordinary_edge = stage.get_points(graph.edge("a", "b").unwrap()).unwrap();
        assert_point_close(ordinary_edge[0], stage.get_center(node));
        assert_point_close(
            *ordinary_edge.last().unwrap(),
            stage.get_center(graph.node("b").unwrap()),
        );
        if alpha == 0.5 {
            let bytes = family_bytes(&stage, graph.mobject());
            if let Some(previous) = &midpoint {
                assert_eq!(&bytes, previous, "seeking must be absolute");
            } else {
                midpoint = Some(bytes);
            }
        }
    }
    transition.finish(&mut stage);
    transition.clean_up_from_scene(&mut stage);
    assert_eq!(stage.roots(), &[graph.mobject()]);
    assert!(!stage.is_animating(graph.mobject()));
    assert!(!stage.is_updating_suspended(graph.mobject()));

    stage.restore(&before);
    transition.validate_begin(&stage).unwrap();
    transition.begin(&mut stage).unwrap();
    transition.interpolate(&mut stage, 0.5);
    assert_eq!(family_bytes(&stage, graph.mobject()), midpoint.unwrap());
    transition.finish(&mut stage);
    transition.clean_up_from_scene(&mut stage);
    assert_eq!(stage.roots(), &[graph.mobject()]);
}

#[test]
fn live_binding_keeps_labels_and_replays_layout_after_snapshot_restore() {
    let mut stage = Stage::new();
    let network = NetworkGraph::new(sample_graph())
        .laid_out(&GraphLayout::Circular)
        .unwrap();
    let graph = network.add_to(&mut stage).unwrap();
    assert!(
        stage.roots().is_empty(),
        "allocation is separate from scene membership"
    );
    assert_eq!(graph.nodes(), network.nodes());
    assert_eq!(graph.edge("a", "b"), graph.edge("b", "a"));
    assert_eq!(graph.edge("a", "c"), None);
    assert_eq!(graph.node("missing"), None);
    let mut transition = LayoutTransition::new(
        &stage,
        &graph,
        &GraphLayout::BreadthFirst { root: "a".into() },
    )
    .unwrap();
    let before = stage.snapshot();
    transition.begin(&mut stage).unwrap();
    transition.interpolate(&mut stage, 0.5);
    let first = family_bytes(&stage, graph.mobject());
    transition.finish(&mut stage);
    stage.restore(&before);
    // The target allocated by the earlier begin no longer exists. It is not
    // an authored preflight handle, and a new live target must be prepared.
    transition.validate_begin(&stage).unwrap();
    transition.begin(&mut stage).unwrap();
    transition.interpolate(&mut stage, 0.5);
    assert_eq!(family_bytes(&stage, graph.mobject()), first);
    assert_edges_connected(&stage, &network, &graph);
}

#[test]
fn graph_animation_rejects_changed_topology_and_invalid_layout_without_mutation() {
    let mut stage = Stage::new();
    let graph = NetworkGraph::new(sample_graph())
        .laid_out(&GraphLayout::Circular)
        .unwrap()
        .add_to(&mut stage)
        .unwrap();
    let mut animation = LayoutTransition::new(&stage, &graph, &GraphLayout::Circular).unwrap();
    animation.state_mut().config.lag_ratio = 0.2;
    let before = stage.snapshot_bytes().unwrap();
    assert!(matches!(
        animation.begin(&mut stage),
        Err(AnimError::InvalidFramePhase(_))
    ));
    assert_eq!(stage.snapshot_bytes().unwrap(), before);
    assert!(matches!(
        LayoutTransition::new(
            &stage,
            &graph,
            &GraphLayout::Spring {
                seed: 0,
                iterations: 1,
                ideal_edge: f64::NAN
            }
        ),
        Err(GraphAnimationError::InvalidControl(_))
    ));
    assert_eq!(stage.snapshot_bytes().unwrap(), before);
    animation.state_mut().config.lag_ratio = 0.0;
    stage.detach(graph.mobject(), graph.node("a").unwrap());
    let changed = stage.snapshot_bytes().unwrap();
    assert_eq!(
        animation.begin(&mut stage),
        Err(AnimError::Stage(StageError::FamilyShapeMismatch))
    );
    assert_eq!(stage.snapshot_bytes().unwrap(), changed);
}

fn traversal_network() -> NetworkGraph {
    // The deliberately unsorted neighbors distinguish fnx insertion order
    // from alphabetical order, and the cycle distinguishes BFS from DFS.
    NetworkGraph::from_edge_list(
        &["r", "z", "a", "m", "b", "isolated"],
        &[
            ("r", "z"),
            ("r", "a"),
            ("z", "m"),
            ("a", "b"),
            ("m", "b"),
            ("r", "r"),
        ],
    )
    .laid_out(&GraphLayout::Circular)
    .unwrap()
}

#[test]
fn host_animation_planners_need_no_layout_or_stage_and_share_native_schedules() {
    let network = NetworkGraph::from_edge_list(
        &["r", "z", "a", "isolated"],
        &[("r", "z"), ("r", "a"), ("z", "a"), ("r", "r")],
    );
    assert!(network.position("r").is_none());
    assert_eq!(
        network
            .traversal_order("r", GraphTraversal::BreadthFirst)
            .unwrap(),
        ["r", "z", "a"]
    );
    assert_eq!(
        network
            .traversal_order("isolated", GraphTraversal::DepthFirst)
            .unwrap(),
        ["isolated"]
    );
    let visits = [("z", "r", 1.0), ("r", "r", 3.0)];
    assert_eq!(network.flow_durations(&visits).unwrap(), [0.25, 0.75]);
    assert_eq!(
        network
            .flow_durations(&[("z", "r", 1e308), ("r", "r", 1e308)])
            .unwrap(),
        [0.5, 0.5]
    );
    assert!(matches!(
        network.flow_durations(&[("r", "isolated", 1.0)]),
        Err(GraphAnimationError::MissingEdge { .. })
    ));
    let mut stage = Stage::new();
    let graph = network
        .laid_out(&GraphLayout::Circular)
        .unwrap()
        .add_to(&mut stage)
        .unwrap();
    let flow = FlowAlongEdges::new(&mut stage, &graph, &visits).unwrap();
    assert_eq!(flow.timings()[0].end - flow.timings()[0].start, 0.25);
    assert_eq!(flow.timings()[1].end - flow.timings()[1].start, 0.75);
}

#[test]
fn traversal_matches_fnx_neighbor_order_and_highlights_only_visited_nodes() {
    let network = traversal_network();
    for (kind, expected) in [
        (GraphTraversal::BreadthFirst, ["r", "z", "a", "m", "b"]),
        (GraphTraversal::DepthFirst, ["r", "z", "m", "b", "a"]),
    ] {
        let mut stage = Stage::new();
        let graph = network.add_to(&mut stage).unwrap();
        let original = rgba(&stage, graph.node("r").unwrap(), "fill_rgba");
        let edge = graph.edge("r", "z").unwrap();
        let edge_before = family_bytes(&stage, edge);
        let mut traversal = TraverseGraph::with_config(
            &mut stage,
            &graph,
            "r",
            kind,
            GraphTraversalConfig {
                run_time: 3.0,
                color: Srgb {
                    r: 0.25,
                    g: 0.5,
                    b: 0.75,
                },
            },
        )
        .unwrap();
        assert_eq!(traversal.order(), &expected);
        traversal.begin(&mut stage).unwrap();
        for completed in 0..=expected.len() {
            traversal.interpolate(&mut stage, completed as f64 / expected.len() as f64);
            for (index, label) in expected.iter().enumerate() {
                let actual = rgba(&stage, graph.node(label).unwrap(), "fill_rgba");
                if index < completed {
                    assert_eq!(
                        &actual[..3],
                        &[0.25, 0.5, 0.75],
                        "{label}, completed={completed}"
                    );
                } else {
                    assert_eq!(actual, original, "{label}, completed={completed}");
                }
            }
            assert_eq!(
                rgba(&stage, graph.node("isolated").unwrap(), "fill_rgba"),
                original
            );
            assert_eq!(family_bytes(&stage, edge), edge_before);
        }
        assert!(traversal.freeze_interpolation().is_some());
        traversal.finish(&mut stage);
    }
}

#[test]
fn traversal_handles_isolated_roots_and_refuses_unknown_roots_and_controls() {
    let mut stage = Stage::new();
    let graph = traversal_network().add_to(&mut stage).unwrap();
    let traversal =
        TraverseGraph::new(&mut stage, &graph, "isolated", GraphTraversal::BreadthFirst).unwrap();
    assert_eq!(traversal.order(), &["isolated"]);
    let before = stage.snapshot_bytes().unwrap();
    assert!(matches!(
        TraverseGraph::new(&mut stage, &graph, "missing", GraphTraversal::DepthFirst),
        Err(GraphAnimationError::Graph(
            fmn_library::NetworkGraphError::UnknownNode(_)
        ))
    ));
    for run_time in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(matches!(
            TraverseGraph::with_config(
                &mut stage,
                &graph,
                "r",
                GraphTraversal::BreadthFirst,
                GraphTraversalConfig {
                    run_time,
                    ..GraphTraversalConfig::default()
                }
            ),
            Err(GraphAnimationError::InvalidControl(_))
        ));
    }
    assert_eq!(stage.snapshot_bytes().unwrap(), before);
}

fn opacity(stage: &Stage, pulse: Mob) -> f32 {
    rgba(stage, pulse, "stroke_rgba")[3]
}

fn edge_fraction(point: Vec3, start: Vec3, end: Vec3) -> f64 {
    let direction: Vec3 = std::array::from_fn(|axis| end[axis] - start[axis]);
    let length_squared: f64 = direction.iter().map(|value| value * value).sum();
    direction
        .iter()
        .enumerate()
        .map(|(axis, value)| (point[axis] - start[axis]) * value)
        .sum::<f64>()
        / length_squared
}

#[test]
fn weighted_flow_uses_live_directed_paths_and_removes_only_its_overlay() {
    let mut stage = Stage::new();
    let graph = NetworkGraph::from_edge_list(&[], &[("a", "b"), ("b", "c")])
        .laid_out(&GraphLayout::Circular)
        .unwrap()
        .add_to(&mut stage)
        .unwrap();
    stage.add_to_scene(graph.mobject()).unwrap();
    let mut flow = FlowAlongEdges::with_config(
        &mut stage,
        &graph,
        &[("b", "a", 1.0), ("b", "c", 3.0)],
        GraphFlowConfig {
            run_time: 4.0,
            ..GraphFlowConfig::default()
        },
    )
    .unwrap();
    assert_eq!(flow.timings()[0].start, 0.0);
    assert_eq!(flow.timings()[0].end, 0.25);
    assert_eq!(flow.timings()[1].start, 0.25);
    assert_eq!(flow.timings()[1].end, 1.0);
    let overlay = flow.state().mobject();
    stage.add_to_scene(overlay).unwrap();
    stage.shift(graph.mobject(), [3.0, 2.0, 0.5]);
    let unchanged = family_bytes(&stage, graph.mobject());
    let start = stage.get_center(graph.node("b").unwrap());
    let end = stage.get_center(graph.node("a").unwrap());
    let pulses: Vec<_> = flow.pulses().collect();
    flow.begin(&mut stage).unwrap();
    assert!(pulses.iter().all(|&pulse| opacity(&stage, pulse) == 0.0));
    for (alpha, earlier_half) in [(0.0625, true), (0.1875, false), (0.0625, true)] {
        flow.interpolate(&mut stage, alpha);
        assert!(opacity(&stage, pulses[0]) > 0.0);
        assert_eq!(opacity(&stage, pulses[1]), 0.0);
        let fraction = edge_fraction(stage.get_center(pulses[0]), start, end);
        assert_eq!(fraction < 0.5, earlier_half, "pulse fraction={fraction}");
        assert_eq!(family_bytes(&stage, graph.mobject()), unchanged);
    }
    flow.interpolate(&mut stage, 0.625);
    assert_eq!(opacity(&stage, pulses[0]), 0.0);
    assert!(opacity(&stage, pulses[1]) > 0.99);
    assert_eq!(family_bytes(&stage, graph.mobject()), unchanged);
    flow.finish(&mut stage);
    assert!(pulses.iter().all(|&pulse| opacity(&stage, pulse) == 0.0));
    flow.clean_up_from_scene(&mut stage);
    assert_eq!(stage.roots(), &[graph.mobject()]);
    assert_eq!(family_bytes(&stage, graph.mobject()), unchanged);
    // Cleanup must preserve its own family too, so reuse does not strand
    // detached pulses or keep the prior finish's zero opacity as the start.
    stage.add_to_scene(overlay).unwrap();
    flow.begin(&mut stage).unwrap();
    flow.interpolate(&mut stage, 0.125);
    assert!(opacity(&stage, pulses[0]) > 0.99);
    flow.finish(&mut stage);
    flow.clean_up_from_scene(&mut stage);
    assert_eq!(stage.roots(), &[graph.mobject()]);
}

#[test]
fn graph_flow_preserves_contours_and_normalizes_reversed_end_handles() {
    let source = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [10.0, 4.0, 0.0],
        [12.0, 4.0, 0.0],
        [12.0, 4.0, 0.0],
    ];
    let reversed = [
        [12.0, 4.0, 0.0],
        [11.0, 4.0, 0.0],
        [10.0, 4.0, 0.0],
        [10.0, 4.0, 0.0],
        [2.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
    ];
    for (from, to, expected) in [("a", "b", source), ("b", "a", reversed)] {
        let mut stage = Stage::new();
        let graph = NetworkGraph::from_edge_list(&["a", "b"], &[("a", "b")])
            .laid_out(&GraphLayout::Circular)
            .unwrap()
            .add_to(&mut stage)
            .unwrap();
        stage.add_to_scene(graph.mobject()).unwrap();
        let edge = graph.edge("a", "b").unwrap();
        let mut flow = FlowAlongEdges::with_config(
            &mut stage,
            &graph,
            &[(from, to, 1.0)],
            GraphFlowConfig {
                time_width: 2.0,
                ..GraphFlowConfig::default()
            },
        )
        .unwrap();
        let overlay = flow.state().mobject();
        stage.add_to_scene(overlay).unwrap();
        // Use the edge as authored at begin, including the straight segment's
        // end-coincident handle and the gap between the two contours.
        stage.set_points(edge, &source).unwrap();
        let unchanged = family_bytes(&stage, graph.mobject());
        let pulse = flow.pulses().next().unwrap();
        flow.begin(&mut stage).unwrap();
        assert_eq!(opacity(&stage, pulse), 0.0);
        flow.interpolate(&mut stage, 0.5);
        assert!(opacity(&stage, pulse) > 0.99);
        // The wide window exposes the full path here, isolating direction and
        // contour topology from partial-path clipping.
        let path = fmn_geom::QuadPath::from_points(stage.get_points(pulse).unwrap()).unwrap();
        assert_eq!(path.points(), expected.as_slice());
        let contours = path.subpaths();
        assert_eq!(contours.len(), 2);
        assert_eq!(contours[0], &expected[..3]);
        assert_eq!(contours[1], &expected[4..]);
        assert_eq!(
            stage.get_points(edge).unwrap().as_slice(),
            source.as_slice()
        );
        assert_eq!(family_bytes(&stage, graph.mobject()), unchanged);

        flow.finish(&mut stage);
        assert_eq!(opacity(&stage, pulse), 0.0);
        flow.clean_up_from_scene(&mut stage);
        assert_eq!(stage.roots(), &[graph.mobject()]);
        assert_eq!(family_bytes(&stage, graph.mobject()), unchanged);
    }
}

#[test]
fn certified_raster_preserves_graph_flow_gaps_and_self_loops() {
    use fmn_core::color::LinearRgba;
    use fmn_core::constants::WHITE;
    use fmn_library::{Style, VMobject};
    use fmn_render::{
        EngineIdentity, FrameConfig, RetainedFrameRenderer, RetainedFrameRendererConfig, ScreenMap,
        Tiling, Viewport,
    };

    let source = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [10.0, 4.0, 0.0],
        [11.0, 4.0, 0.0],
        [12.0, 4.0, 0.0],
    ];
    let mut serial_frames = None;
    for threads in [1, 4, 16] {
        let mut renderer = RetainedFrameRenderer::new(RetainedFrameRendererConfig {
            frame: FrameConfig::new(
                Viewport {
                    width: 256,
                    height: 128,
                },
                ScreenMap {
                    scale: 16.0,
                    origin: [32.0, 96.0],
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
            threads,
        })
        .unwrap();
        let render = |renderer: &mut RetainedFrameRenderer, stage: &Stage| {
            renderer.render(stage, 0).unwrap();
            renderer.frame().plane(0).to_vec()
        };
        let mut observed = Vec::new();
        for (from, to, expected_points) in [
            (
                "a",
                "b",
                [[10.0, 4.0, 0.0], [10.34375, 4.0, 0.0], [10.6875, 4.0, 0.0]],
            ),
            (
                "b",
                "a",
                [[2.0, 0.0, 0.0], [1.65625, 0.0, 0.0], [1.3125, 0.0, 0.0]],
            ),
        ] {
            let mut stage = Stage::new();
            let graph = NetworkGraph::from_edge_list(&["a", "b"], &[("a", "b")])
                .laid_out(&GraphLayout::Circular)
                .unwrap()
                .add_to(&mut stage)
                .unwrap();
            stage.add_to_scene(graph.mobject()).unwrap();
            stage
                .set_points(graph.edge("a", "b").unwrap(), &source)
                .unwrap();
            let graph_before = family_bytes(&stage, graph.mobject());
            let baseline = render(&mut renderer, &stage);

            // At alpha 5/8 the window is [17/32, 25/32]. Its upper
            // boundary reveals 11/32 of the third curve. Author that one
            // straight contour directly, without calling partial_points or
            // any animation; there_and_back(5/8) = smooth(3/4) = 459/512.
            let mut expected_stage = stage.snapshot().materialize();
            let expected_pulse = expected_stage.add(
                VMobject::from_points(expected_points.to_vec())
                    .with_style(Style::default().stroke(WHITE, 5.0, 459.0 / 512.0)),
            );
            expected_stage.add_to_scene(expected_pulse).unwrap();
            let expected = render(&mut renderer, &expected_stage);
            assert!(
                expected != baseline,
                "the reference pulse must paint pixels"
            );

            let mut flow = FlowAlongEdges::with_config(
                &mut stage,
                &graph,
                &[(from, to, 1.0)],
                GraphFlowConfig {
                    color: WHITE,
                    stroke_width: 5.0,
                    time_width: 0.25,
                    ..GraphFlowConfig::default()
                },
            )
            .unwrap();
            stage.add_to_scene(flow.state().mobject()).unwrap();
            flow.begin(&mut stage).unwrap();
            for alpha in [0.5, 0.625, 0.5] {
                flow.interpolate(&mut stage, alpha);
                let frame = render(&mut renderer, &stage);
                let reference = if alpha == 0.5 { &baseline } else { &expected };
                assert!(
                    &frame == reference,
                    "threads={threads}, flow={from}->{to}, alpha={alpha}"
                );
                assert_eq!(family_bytes(&stage, graph.mobject()), graph_before);
                observed.push(frame);
            }
            flow.finish(&mut stage);
            flow.clean_up_from_scene(&mut stage);
            assert_eq!(stage.roots(), &[graph.mobject()]);
            let finished = render(&mut renderer, &stage);
            assert!(finished == baseline, "cleanup must restore the graph image");
            observed.push(finished);
        }

        let mut stage = Stage::new();
        let graph = NetworkGraph::from_edge_list(&["a", "b"], &[("a", "a"), ("a", "b")])
            .laid_out(&GraphLayout::Circular)
            .unwrap()
            .add_to(&mut stage)
            .unwrap();
        stage.add_to_scene(graph.mobject()).unwrap();
        let self_loop = graph.edge("a", "a").unwrap();
        let mut transition = LayoutTransition::new(
            &stage,
            &graph,
            &GraphLayout::BreadthFirst { root: "a".into() },
        )
        .unwrap();
        transition.begin(&mut stage).unwrap();
        for alpha in [0.0, 1.0] {
            transition.interpolate(&mut stage, alpha);
            let visible = render(&mut renderer, &stage);
            let mut without_loop = stage.snapshot().materialize();
            without_loop.set_color_lanes(self_loop, "stroke_rgba", None, Some(0.0), false);
            let hidden = render(&mut renderer, &without_loop);
            assert!(
                visible != hidden,
                "self-loop must paint visible pixels before and after layout, alpha={alpha}"
            );
            observed.push(visible);
            observed.push(hidden);
        }
        transition.finish(&mut stage);
        transition.clean_up_from_scene(&mut stage);
        if let Some(serial) = &serial_frames {
            assert!(
                observed == *serial,
                "certified bytes differ at {threads} threads"
            );
        } else {
            serial_frames = Some(observed);
        }
    }
}

#[test]
fn flow_validates_all_edges_and_weights_before_allocating_and_handles_large_weights() {
    let mut stage = Stage::new();
    let graph = NetworkGraph::new(sample_graph())
        .laid_out(&GraphLayout::Circular)
        .unwrap()
        .add_to(&mut stage)
        .unwrap();
    let before = stage.snapshot_bytes().unwrap();
    assert!(matches!(
        FlowAlongEdges::new(&mut stage, &graph, &[]),
        Err(GraphAnimationError::EmptyFlow)
    ));
    assert!(matches!(
        FlowAlongEdges::new(&mut stage, &graph, &[("a", "b", 1.0), ("a", "c", 1.0)]),
        Err(GraphAnimationError::MissingEdge { .. })
    ));
    for weight in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(matches!(
            FlowAlongEdges::new(&mut stage, &graph, &[("a", "b", 1.0), ("b", "c", weight)]),
            Err(GraphAnimationError::InvalidControl(_))
        ));
    }
    assert!(matches!(
        FlowAlongEdges::new(
            &mut stage,
            &graph,
            &[("a", "b", f64::MAX), ("b", "c", f64::MIN_POSITIVE)]
        ),
        Err(GraphAnimationError::InvalidControl(_))
    ));
    assert_eq!(stage.snapshot_bytes().unwrap(), before);
    let flow = FlowAlongEdges::new(
        &mut stage,
        &graph,
        &[("a", "b", f64::MAX), ("b", "c", f64::MAX)],
    )
    .unwrap();
    assert_eq!(flow.timings()[0].duration(), 0.5);
    assert_eq!(flow.timings()[1].duration(), 0.5);
}

#[test]
fn graph_animation_workers_reproduce_seconds_and_exact_frame_sampling() {
    use fmn_anim::pure_frame::PureSegment;
    use fmn_anim::{RationalFrameClock, advance_play, finish_open_play, open_play};
    use fmn_core::rng::RngRoot;

    for (kind, run_time_frames, shaped) in [
        (0, None, false),
        (1, None, false),
        (0, Some(43), false),
        (1, Some(43), false),
        (1, None, true),
        (1, Some(43), true),
    ] {
        let mut stage = Stage::new();
        let graph = NetworkGraph::new(sample_graph())
            .laid_out(&GraphLayout::Circular)
            .unwrap()
            .add_to(&mut stage)
            .unwrap();
        stage.add_to_scene(graph.mobject()).unwrap();
        let mut animation: Box<dyn Animation> = match kind {
            1 => Box::new(
                TraverseGraph::new(&mut stage, &graph, "a", GraphTraversal::DepthFirst).unwrap(),
            ),
            _ => Box::new(
                LayoutTransition::new(
                    &stage,
                    &graph,
                    &GraphLayout::BreadthFirst { root: "a".into() },
                )
                .unwrap(),
            ),
        };
        animation.state_mut().config.run_time_frames = run_time_frames;
        if shaped {
            animation.state_mut().config.rate_func =
                RateFunc::Sampled(Arc::from([0.0, 0.9, 0.1, 0.8, 1.0]));
            animation.state_mut().config.time_span = Some((0.03125, 0.4375));
        }
        let mut animations = vec![animation];
        let fps = if run_time_frames.is_some() { 24 } else { 8 };
        let mut clock = RationalFrameClock::new(fps).unwrap();
        let rng = RngRoot::from_seed(73);
        let mut open = open_play(&mut stage, &clock, &mut animations).unwrap();
        let pure = PureSegment::freeze(&stage, &clock, &rng, &animations, &open).unwrap();
        if let Some(frames) = run_time_frames {
            assert_eq!(pure.frame_count(), i64::try_from(frames).unwrap());
            let duration = animations[0].duration_on_grid(fps).unwrap();
            assert!(duration.is_frame_native());
            assert_eq!(duration.n_frames().unwrap(), i64::try_from(frames).unwrap());
            for frame in 1..=43 {
                assert_eq!(
                    animations[0]
                        .state()
                        .frame_alpha(f64::from(frame), fps)
                        .to_bits(),
                    (f64::from(frame) / 43.0).to_bits(),
                    "kind={kind}, frame={frame}"
                );
            }
            if kind != 0 {
                let timing = animations[0]
                    .composition_timing()
                    .expect("graph wrappers retain their prepared composition timing");
                // Only the outer duration was overridden. All children were
                // authored in seconds, so their interpolation windows retain
                // that coordinate system while the group consumes 43 frames.
                assert!(!timing.is_frame_native());
                assert_eq!(timing.intervals().len(), graph.nodes().len());
                assert_eq!(timing.max_end(), 5.0);
                if !shaped {
                    for frame in 0..=43 {
                        let frame = f64::from(frame);
                        assert_eq!(
                            timing.rate_alpha_at_frame(frame, fps).to_bits(),
                            (frame / 43.0).to_bits()
                        );
                        assert_eq!(
                            timing.position_at_frame(frame, fps).to_bits(),
                            (frame * timing.max_end() / 43.0).to_bits()
                        );
                    }
                }
            }
        }
        let mut expected = Vec::new();
        advance_play(
            &mut stage,
            &mut clock,
            &rng,
            &mut animations,
            &mut open,
            pure.frame_count(),
            &mut |packet| {
                expected.push(rooted_bytes(&packet.materialize_stage()));
            },
        )
        .unwrap();
        assert_eq!(expected.len(), usize::try_from(pure.frame_count()).unwrap());
        assert_eq!(clock.now().frames(), pure.frame_count());
        std::thread::scope(|scope| {
            for worker in 0..4 {
                let pure = &pure;
                let expected = &expected;
                scope.spawn(move || {
                    for index in (worker..expected.len()).step_by(4).rev() {
                        let materialized = pure.job(index as i64 + 1).unwrap().materialize();
                        assert_eq!(
                            rooted_bytes(&materialized),
                            expected[index],
                            "kind={kind}, frames={run_time_frames:?}, shaped={shaped}, frame={index}"
                        );
                    }
                });
            }
        });
        finish_open_play(&mut stage, &mut animations, open).unwrap();
        assert_eq!(stage.roots(), &[graph.mobject()]);
    }
}

#[test]
fn exact_frame_flow_preserves_serial_frames_when_pure_dispatch_falls_back() {
    use fmn_anim::pure_frame::{PlayCapture, PureSegment, play_segment_with_pure_dispatch};
    use fmn_anim::{FramePacket, RationalFrameClock};
    use fmn_core::rng::RngRoot;

    #[derive(Default)]
    struct Capture {
        packets: Vec<FramePacket>,
        programs: usize,
    }
    impl PlayCapture for Capture {
        fn capture(&mut self, packet: FramePacket) {
            self.packets.push(packet);
        }
        fn accepts_pure_segments(&self) -> bool {
            true
        }
        fn capture_pure_segment(&mut self, _segment: &PureSegment) -> bool {
            self.programs += 1;
            false
        }
    }

    let mut initial = Stage::new();
    let graph = NetworkGraph::new(sample_graph())
        .laid_out(&GraphLayout::Circular)
        .unwrap()
        .add_to(&mut initial)
        .unwrap();
    initial.add_to_scene(graph.mobject()).unwrap();
    let baseline = initial.snapshot();
    let graph_before = family_bytes(&initial, graph.mobject());
    let mut expected = None;
    for allow_pure in [false, true] {
        let mut stage = baseline.materialize();
        let mut flow =
            FlowAlongEdges::new(&mut stage, &graph, &[("a", "b", 29.0), ("b", "c", 38.0)]).unwrap();
        flow.state_mut().config.run_time_frames = Some(43);
        let overlay = flow.state().mobject();
        let pulses: Vec<_> = flow.pulses().collect();
        assert_eq!(pulses.len(), 2);
        stage.add_to_scene(overlay).unwrap();
        let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(flow)];
        let mut clock = RationalFrameClock::new(24).unwrap();
        clock.advance_frames(5).unwrap();
        stage.set_time_from_clock(clock.now().to_f64());
        let mut capture = Capture::default();
        let report = play_segment_with_pure_dispatch(
            &mut stage,
            &mut clock,
            &RngRoot::from_seed(73),
            &mut animations,
            false,
            allow_pure,
            &mut |_, _| {},
            &mut capture,
        )
        .unwrap();
        // Flash geometry and opacity share each pulse. The object-level
        // overlap gate must keep their serial write order, even for a sink
        // that accepts pure programs; refusal must still emit every frame.
        assert_eq!(capture.programs, 0);
        assert_eq!(capture.packets.len(), 43);
        assert_eq!((report.base_frame, report.n_frames), (5, 43));
        assert_eq!(report.run_time, 43.0 / 24.0);
        assert_eq!(clock.now().frames(), 48);
        let duration = animations[0].duration_on_grid(24).unwrap();
        assert!(duration.is_frame_native());
        assert_eq!(duration.n_frames().unwrap(), 43);
        let timing = animations[0].composition_timing().unwrap();
        assert!(!timing.is_frame_native());
        assert_eq!(timing.intervals().len(), 2);
        assert_eq!(timing.max_end(), 1.0);
        let mut visible = [false; 2];
        let mut observed = Vec::new();
        for (index, packet) in capture.packets.iter().enumerate() {
            let frame = u32::try_from(index + 1).unwrap();
            let alpha = f64::from(frame) / 43.0;
            assert_eq!(packet.segment_frame(), i64::from(frame));
            assert_eq!(packet.frame_index(), 5 + i64::from(frame));
            assert_eq!(packet.time().frames(), packet.frame_index());
            assert_eq!(packet.time().fps(), 24);
            assert_eq!(packet.alpha().to_bits(), alpha.to_bits());
            assert_eq!(
                timing.position_at_frame(f64::from(frame), 24).to_bits(),
                alpha.to_bits()
            );
            let sampled = packet.materialize_stage();
            assert_eq!(sampled.roots(), &[graph.mobject(), overlay]);
            assert_eq!(family_bytes(&sampled, graph.mobject()), graph_before);
            for (seen, &pulse) in visible.iter_mut().zip(&pulses) {
                *seen |= opacity(&sampled, pulse) > 0.0;
            }
            observed.push(rooted_bytes(&sampled));
        }
        assert!(visible.into_iter().all(|seen| seen));
        assert_ne!(observed.first(), observed.last());
        assert_eq!(stage.roots(), &[graph.mobject()]);
        assert_eq!(family_bytes(&stage, graph.mobject()), graph_before);
        assert!(pulses.iter().all(|&pulse| opacity(&stage, pulse) == 0.0));
        assert!(animations[0].deferred_error().is_none());
        if let Some(expected) = &expected {
            assert_eq!(&observed, expected);
        } else {
            expected = Some(observed);
        }
    }
}

#[test]
fn edge_list_infers_vertices_without_panicking() {
    let graph = NetworkGraph::from_edge_list(&["isolated"], &[("b", "a"), ("a", "c")])
        .laid_out(&GraphLayout::Circular)
        .unwrap();
    assert_eq!(graph.nodes(), &["isolated", "b", "a", "c"]);
    assert_eq!(graph.build().unwrap().children().len(), 6);
}

#[test]
fn breadth_first_includes_disconnected_vertices_on_an_outer_ring() {
    let graph = NetworkGraph::from_edge_list(&["a", "b", "c", "d"], &[("a", "b")])
        .laid_out(&GraphLayout::BreadthFirst { root: "a".into() })
        .unwrap();
    assert_eq!(graph.position("a"), Some([0.0, 0.0, 0.0]));
    assert_eq!(graph.position("b"), Some([1.0, 0.0, 0.0]));
    assert_eq!(graph.position("c"), Some([1.5, 0.0, 0.0]));
    let d = graph.position("d").unwrap();
    assert!((d[0] + 1.5).abs() < 1e-12 && d[1].abs() < 1e-12);
    assert_eq!(graph.build().unwrap().children().len(), 5);
}
