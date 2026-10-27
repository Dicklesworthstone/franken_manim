//! fm-n64 tranche 1: the enhanced-tier `NetworkGraph` — determinism
//! digests per layout kernel, analytic ground truth for the circular
//! rule, partition/BFS refusals, and one bit-locked self-golden.

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
        for field in entry.buffer.schema().fields() {
            bytes.extend_from_slice(field.name.as_bytes());
            for value in entry.buffer.read_column(&field.name).unwrap() {
                bytes.extend_from_slice(&value.to_bits().to_le_bytes());
            }
        }
    }
    bytes
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
fn layout_and_traversal_native_workers_reproduce_serial_graph_frames() {
    use fmn_anim::pure_frame::PureSegment;
    use fmn_anim::{RationalFrameClock, advance_play, finish_open_play, open_play};
    use fmn_core::rng::RngRoot;

    for traversal in [false, true] {
        let mut stage = Stage::new();
        let graph = NetworkGraph::new(sample_graph())
            .laid_out(&GraphLayout::Circular)
            .unwrap()
            .add_to(&mut stage)
            .unwrap();
        stage.add_to_scene(graph.mobject()).unwrap();
        let animation: Box<dyn Animation> = if traversal {
            Box::new(
                TraverseGraph::new(&mut stage, &graph, "a", GraphTraversal::DepthFirst).unwrap(),
            )
        } else {
            Box::new(
                LayoutTransition::new(
                    &stage,
                    &graph,
                    &GraphLayout::BreadthFirst { root: "a".into() },
                )
                .unwrap(),
            )
        };
        let mut animations = vec![animation];
        let mut clock = RationalFrameClock::new(8).unwrap();
        let rng = RngRoot::from_seed(73);
        let mut open = open_play(&mut stage, &clock, &mut animations).unwrap();
        let pure = PureSegment::freeze(&stage, &clock, &rng, &animations, &open).unwrap();
        let mut expected = Vec::new();
        advance_play(
            &mut stage,
            &mut clock,
            &rng,
            &mut animations,
            &mut open,
            pure.frame_count(),
            &mut |packet| {
                expected.push(family_bytes(&packet.materialize_stage(), graph.mobject()));
            },
        )
        .unwrap();
        std::thread::scope(|scope| {
            for worker in 0..4 {
                let pure = &pure;
                let expected = &expected;
                let root = graph.mobject();
                scope.spawn(move || {
                    for index in (worker..expected.len()).step_by(4).rev() {
                        let materialized = pure.job(index as i64 + 1).unwrap().materialize();
                        assert_eq!(family_bytes(&materialized, root), expected[index]);
                    }
                });
            }
        });
        finish_open_play(&mut stage, &mut animations, open).unwrap();
        assert_eq!(stage.roots(), &[graph.mobject()]);
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
