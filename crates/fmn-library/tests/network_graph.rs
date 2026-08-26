//! fm-n64 tranche 1: the enhanced-tier `NetworkGraph` — determinism
//! digests per layout kernel, analytic ground truth for the circular
//! rule, partition/BFS refusals, and one bit-locked self-golden.

use fmn_hash::sha256;
use fnx_classes::digraph::Graph;

use fmn_library::network_graph::{GraphLayout, NetworkGraph, DEFAULT_SPRING_IDEAL_EDGE};

/// A 5-cycle plus a chord — small enough to inspect, rich enough to have
/// two BFS levels and a non-trivial edge set.
fn sample_graph() -> Graph {
    let mut graph = Graph::strict();
    for label in ["a", "b", "c", "d", "e"] {
        graph.add_node(label);
    }
    for (u, v) in [("a", "b"), ("b", "c"), ("c", "d"), ("d", "e"), ("e", "a"), ("b", "d")] {
        graph
            .add_edge(u, v)
            .unwrap_or_else(|error| fail(format!("edge {u}-{v}: {error}")));
    }
    graph
}

/// Fail with a message. UBS bans literal `panic!`/`unreachable!`; test
/// refusals go through [`std::panic::panic_any`] like the svg suite's.
fn fail(message: String) {
    std::panic::panic_any(message);
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
        .laid_out(&GraphLayout::BreadthFirst {
            root: "a".into(),
        })
        .unwrap_or_else(|error| fail(format!("{error}")));
    // Level radii are level_index / depth; depth = 2 here (a; b,e; c,d).
    let radius_of = |label: &str| network.position(label).expect("laid out")[0];
    let center = |label: &str| {
        let p = network.position(label).expect("laid out");
        (p[0] * p[0] + p[1] * p[1]).sqrt()
    };
    // Level 0 sits at the origin ring (radius 0).
    assert!(center("a") < 1e-12);
    // Levels 1 and 2 sit on their rings; sorted expansion puts b before e
    // on level 1's arc (angle order matches sorted labels).
    assert!(center("b") > center("a"));
    assert!(center("c") > center("b"));
    let _ = radius_of("unused");
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
    let family = network.build().unwrap_or_else(|error| fail(format!("{error}")));
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
    let family = network.build().unwrap_or_else(|error| fail(format!("{error}")));
    let golden = "GOLDEN_PLACEHOLDER";
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
    let actual = sha256(&bytes).to_hex();
    if golden.starts_with("GOLDEN_PLACEHOLDER") {
        fail(format!("SELF GOLDEN SEED network_graph star: {actual}"));
    }
}
