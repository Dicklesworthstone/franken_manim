//! Tests for NeuralNetworkMobject (§2.6 leapfrog #9, fm-74z):
//! MLP construction, Conv stack introspection, live-value updaters,
//! purity classification, large-layer elision, and deterministic self-goldens.

use fmn_anim::purity::{ImpureEffect, Purity, classify_wait};
use fmn_core::color::{Srgb, interpolate_color};
use fmn_core::constants::{BLUE_D, GREY_B, RED, YELLOW};
use fmn_hash::sha256;
use fmn_library::VStyle;
use fmn_library::neural_network::{LayerKind, NeuralNetworkConfig, NeuralNetworkMobject};
use fmn_mobject::{Mob, Stage, UpdaterError};

fn fail(message: String) -> ! {
    std::panic::panic_any(message)
}

fn stored_color(color: Srgb) -> Srgb {
    Srgb {
        r: f64::from(color.r as f32),
        g: f64::from(color.g as f32),
        b: f64::from(color.b as f32),
    }
}

type GeometryFacts = Vec<(Mob, Vec<[u64; 3]>, [u64; 12])>;

fn family_geometry(stage: &Stage, root: Mob) -> GeometryFacts {
    stage
        .family(root)
        .into_iter()
        .map(|mob| {
            (
                mob,
                stage
                    .get_points(mob)
                    .unwrap()
                    .into_iter()
                    .map(|point| point.map(f64::to_bits))
                    .collect(),
                stage
                    .get(mob)
                    .unwrap()
                    .placement()
                    .coefficients()
                    .map(f64::to_bits),
            )
        })
        .collect()
}

fn family_revisions(stage: &Stage, root: Mob) -> Vec<(Mob, u64)> {
    stage
        .family(root)
        .into_iter()
        .map(|mob| (mob, stage.get(mob).unwrap().buffer.revision()))
        .collect()
}

#[test]
fn mlp_construction_and_layer_layout() {
    let net =
        NeuralNetworkMobject::from_layer_sizes(&[3, 4, 2]).unwrap_or_else(|e| fail(format!("{e}")));
    assert_eq!(net.layer_count(), 3);
    assert_eq!(net.layers()[0].unit_count(), 3);
    assert_eq!(net.layers()[1].unit_count(), 4);
    assert_eq!(net.layers()[2].unit_count(), 2);

    // Layer 0 is Input, Layer 1 is Dense, Layer 2 is Output
    assert!(matches!(net.layers()[0].kind, LayerKind::Input { size: 3 }));
    assert!(matches!(
        net.layers()[1].kind,
        LayerKind::Dense {
            in_features: 3,
            out_features: 4
        }
    ));
    assert!(matches!(
        net.layers()[2].kind,
        LayerKind::Output { size: 2 }
    ));

    // Check neuron positions
    let p0_0 = net.neuron_position(0, 0).expect("neuron pos");
    let p0_1 = net.neuron_position(0, 1).expect("neuron pos");
    let p0_2 = net.neuron_position(0, 2).expect("neuron pos");

    // All layer 0 neurons share the same x-coordinate
    assert_eq!(p0_0[0], p0_1[0]);
    assert_eq!(p0_1[0], p0_2[0]);
    // y-coordinates decrease from top to bottom
    assert!(p0_0[1] > p0_1[1]);
    assert!(p0_1[1] > p0_2[1]);

    // Build the visual family
    let built = net.build().unwrap_or_else(|e| fail(format!("{e}")));
    // Synapses: (3*4) + (4*2) = 12 + 8 = 20
    // Neurons: 3 + 4 + 2 = 9
    // Total children = 29
    assert_eq!(built.children().len(), 20 + 9);
}

#[test]
fn conv_stack_introspection() {
    let conv_net = NeuralNetworkMobject::introspect_conv_stack(
        3,                             // 3 input channels (RGB)
        &[(16, (3, 3)), (32, (3, 3))], // Conv1(16), Conv2(32)
        &[64, 10],                     // Dense(64), Output(10)
    )
    .unwrap_or_else(|e| fail(format!("{e}")));

    assert_eq!(conv_net.layer_count(), 5);
    assert_eq!(conv_net.layers()[0].label.as_deref(), Some("Input"));
    assert_eq!(conv_net.layers()[1].label.as_deref(), Some("Conv1"));
    assert_eq!(conv_net.layers()[2].label.as_deref(), Some("Conv2"));
    assert_eq!(conv_net.layers()[3].label.as_deref(), Some("Dense"));
    assert_eq!(conv_net.layers()[4].label.as_deref(), Some("Output"));

    assert_eq!(conv_net.layers()[0].unit_count(), 3);
    assert_eq!(conv_net.layers()[1].unit_count(), 16);
    assert_eq!(conv_net.layers()[2].unit_count(), 32);
    assert_eq!(conv_net.layers()[3].unit_count(), 64);
    assert_eq!(conv_net.layers()[4].unit_count(), 10);
}

#[test]
fn introspect_explicit_weights_and_feed_forward() {
    let layer_sizes = [2, 2, 1];
    let weights = vec![
        vec![vec![1.0, -1.0], vec![-1.0, 1.0]], // Layer 0 -> Layer 1
        vec![vec![2.0], vec![2.0]],             // Layer 1 -> Layer 2
    ];
    let biases = vec![
        vec![0.0, 0.0], // Layer 1 biases
        vec![-1.0],     // Layer 2 bias
    ];

    let mut net = NeuralNetworkMobject::introspect_mlp(&layer_sizes, &weights, &biases)
        .unwrap_or_else(|e| fail(format!("{e}")));

    // Test feed forward on inputs [1.0, 0.0]
    // Layer 1:
    // z0 = 1.0*1.0 + 0.0*(-1.0) + 0.0 = 1.0 -> sigmoid(1.0) ≈ 0.7310585786300049
    // z1 = 1.0*(-1.0) + 0.0*1.0 + 0.0 = -1.0 -> sigmoid(-1.0) ≈ 0.2689414213699951
    // Layer 2:
    // z = 0.7310585786300049 * 2.0 + 0.2689414213699951 * 2.0 - 1.0
    //   = 2.0 * 1.0 - 1.0 = 1.0 -> sigmoid(1.0) ≈ 0.7310585786300049
    let out = net
        .feed_forward(&[1.0, 0.0])
        .unwrap_or_else(|e| fail(format!("{e}")));
    assert_eq!(out.len(), 1);
    let expected = 1.0 / (1.0 + (-1.0_f64).exp());
    assert!((out[0] - expected).abs() < 1e-10);

    // Activations are stored in layers
    assert_eq!(net.layers()[0].activations, vec![1.0, 0.0]);
    assert!((net.layers()[1].activations[0] - expected).abs() < 1e-10);
}

#[test]
fn live_value_updaters_and_purity_classification() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let mut stage = Stage::new();
    let net = Rc::new(RefCell::new(
        NeuralNetworkMobject::from_layer_sizes(&[2, 3, 1]).unwrap(),
    ));
    let mut binding = net.borrow().add_to(&mut stage).unwrap();
    let handle = binding.mobject();
    let output = binding.neuron(2, 0).unwrap();
    let synapse = binding.synapse(1, 2, 0).unwrap();
    stage
        .add_to_scene(handle)
        .unwrap_or_else(|e| fail(format!("{e}")));

    // Initially with no updaters, a play/wait is pure
    let initial_purity = classify_wait(&stage, false);
    assert!(initial_purity.is_pure());

    let values = Rc::clone(&net);
    let updater = stage
        .add_try_updater(
            handle,
            move |stage, _mob| {
                binding
                    .sync_values(stage, &values.borrow())
                    .map_err(|error| {
                        UpdaterError::callback("neural live values", error.to_string())
                    })
            },
            false,
        )
        .unwrap_or_else(|e| fail(format!("{e}")));

    // Purity classifier now demotes the scene to stateful!
    let updated_purity = classify_wait(&stage, false);
    assert_eq!(
        updated_purity,
        Purity::Stateful(vec![ImpureEffect::SceneUpdater])
    );

    assert_eq!(stage.get_fill_color(output), Some(stored_color(GREY_B)));
    net.borrow_mut().set_neuron_activation(2, 0, 1.0).unwrap();
    net.borrow_mut().set_synapse_weight(1, 2, 0, -0.75).unwrap();
    stage.update(1.0 / 30.0);
    assert!(stage.updater_error().is_none());
    assert_eq!(stage.get_fill_color(output), Some(stored_color(YELLOW)));
    assert_eq!(stage.get_stroke_color(synapse), Some(stored_color(RED)));

    let revisions = family_revisions(&stage, handle);
    stage.update(1.0 / 30.0);
    assert_eq!(family_revisions(&stage, handle), revisions);
    stage.remove_updater(handle, updater);
    assert!(classify_wait(&stage, false).is_pure());
}

#[test]
fn large_layer_compression_and_elision() {
    // 4096-neuron layer
    let net = NeuralNetworkMobject::from_layer_sizes(&[4, 4096, 2])
        .unwrap_or_else(|e| fail(format!("{e}")));

    assert_eq!(net.layers()[1].unit_count(), 4096);
    let built = net.build().unwrap_or_else(|e| fail(format!("{e}")));

    // Neurons are capped at max_displayed_neurons (12) + 3 ellipsis dots for the 4096 layer
    // Displayed neurons: 4 (input) + 12 (dense) + 2 (output) = 18
    // Ellipsis dots: 3
    // Synapses: 4 * 12 + 12 * 2 = 48 + 24 = 72
    // Total children = 72 + 18 + 3 = 93
    assert_eq!(built.children().len(), 93);
}

#[test]
fn live_values_preserve_handles_geometry_and_scene_placement() {
    let mut stage = Stage::new();
    let mut net = NeuralNetworkMobject::from_layer_sizes(&[2, 3, 1]).unwrap();
    let mut binding = net.add_to(&mut stage).unwrap();
    let root = binding.mobject();
    stage.add_to_scene(root).unwrap();
    stage.scale(root, 1.25);
    stage.rotate(root, 0.37, [0.0, 0.0, 1.0], None, None);
    stage.shift(root, [1.5, -0.75, 0.25]);
    let geometry = family_geometry(&stage, root);
    let neuron = binding.neuron(1, 2).unwrap();
    let synapse = binding.synapse(0, 1, 2).unwrap();
    let unchanged = binding.neuron(0, 0).unwrap();
    stage.set_fill(neuron, None, Some(0.35), None, false);
    stage.set_stroke(neuron, Some(RED), Some(4.5), Some(0.6), Some(true), false);
    let unchanged_revision = stage.get(unchanged).unwrap().buffer.revision();
    let before_revision = stage.get(neuron).unwrap().buffer.revision();

    net.set_neuron_activation(1, 2, 0.8).unwrap();
    net.set_synapse_weight(0, 1, 2, -0.75).unwrap();
    binding.sync_values(&mut stage, &net).unwrap();
    assert_eq!(family_geometry(&stage, root), geometry);
    assert_eq!(binding.neuron(1, 2), Some(neuron));
    assert_eq!(binding.synapse(0, 1, 2), Some(synapse));
    assert_eq!(
        stage.get_fill_color(neuron),
        Some(stored_color(interpolate_color(GREY_B, YELLOW, 0.8)))
    );
    assert_eq!(stage.get_stroke_color(synapse), Some(stored_color(RED)));
    assert_eq!(stage.get_fill_opacity(neuron), Some(f64::from(0.35_f32)));
    assert_eq!(stage.get_stroke_color(neuron), Some(stored_color(RED)));
    assert_eq!(stage.get_stroke_width(neuron), Some(4.5));
    assert_eq!(stage.get_stroke_opacity(neuron), Some(f64::from(0.6_f32)));
    assert!(stage.get(neuron).unwrap().uniforms().stroke_behind);
    assert_eq!(
        stage.get_stroke_width(synapse),
        Some(f64::from((1.6 * (0.4 + 0.6 * 0.75)) as f32))
    );
    assert_eq!(
        stage.get_stroke_opacity(synapse),
        Some(f64::from((0.65 * (0.3 + 0.7 * 0.75)) as f32))
    );
    assert!(stage.get(neuron).unwrap().buffer.revision() > before_revision);
    assert_eq!(
        stage.get(unchanged).unwrap().buffer.revision(),
        unchanged_revision
    );

    let revisions = family_revisions(&stage, root);
    binding.sync_values(&mut stage, &net).unwrap();
    assert_eq!(family_revisions(&stage, root), revisions);

    // Value updates remain meaningful while the family is not in the draw
    // list; removal is not destruction and must not invalidate its binding.
    stage.remove_from_scene(root);
    net.set_synapse_weight(0, 1, 2, 1.0).unwrap();
    binding.sync_values(&mut stage, &net).unwrap();
    assert_eq!(stage.get_stroke_color(synapse), Some(stored_color(BLUE_D)));
    stage.add_to_scene(root).unwrap();
    assert_eq!(family_geometry(&stage, root), geometry);
}

#[test]
fn live_values_refresh_foreign_style_writes_without_model_changes() {
    let mut stage = Stage::new();
    let mut net = NeuralNetworkMobject::from_layer_sizes(&[1, 1]).unwrap();
    net.set_neuron_activation(1, 0, 1.0).unwrap();
    let mut binding = net.add_to(&mut stage).unwrap();
    let neuron = binding.neuron(1, 0).unwrap();
    let last_record = stage.get(neuron).unwrap().buffer.len() - 1;
    let view = stage
        .get_mut(neuron)
        .unwrap()
        .buffer
        .export_field_view("fill_rgba", true)
        .unwrap();
    let revision = stage.get(neuron).unwrap().buffer.revision();
    assert!(view.write_foreign(last_record, "fill_rgba", &[0.0, 0.0, 0.0, 0.125]));
    assert_eq!(stage.get(neuron).unwrap().buffer.revision(), revision);

    binding.sync_values(&mut stage, &net).unwrap();
    let expected = [YELLOW.r as f32, YELLOW.g as f32, YELLOW.b as f32, 0.125];
    assert_eq!(
        stage
            .get(neuron)
            .unwrap()
            .buffer
            .read(last_record, "fill_rgba"),
        Some(expected.to_vec())
    );
    assert_eq!(stage.get_fill_opacity(neuron), Some(1.0));
    let revisions = family_revisions(&stage, binding.mobject());
    binding.sync_values(&mut stage, &net).unwrap();
    assert_eq!(family_revisions(&stage, binding.mobject()), revisions);
}

#[test]
fn compressed_bindings_select_both_ends_and_connect_actual_neuron_indices() {
    for cap in [2_usize, 3, 5, 12] {
        let mut net = NeuralNetworkMobject::from_layer_sizes(&[2, 31, 2])
            .unwrap()
            .with_config(NeuralNetworkConfig {
                max_displayed_neurons: cap,
                ..NeuralNetworkConfig::default()
            });
        let mut stage = Stage::new();
        let mut binding = net.add_to(&mut stage).unwrap();
        let expected: Vec<usize> = (0..cap.div_ceil(2)).chain(31 - cap / 2..31).collect();
        let actual: Vec<usize> = (0..31)
            .filter(|&index| binding.neuron(1, index).is_some())
            .collect();
        assert_eq!(actual, expected, "cap {cap}");
        let mut centers = Vec::new();
        for index in 0..31 {
            let visible = expected.contains(&index);
            assert_eq!(net.neuron_position(1, index).is_some(), visible);
            assert_eq!(binding.synapse(0, 0, index).is_some(), visible);
            assert_eq!(binding.synapse(1, index, 0).is_some(), visible);
            if let Some(neuron) = binding.neuron(1, index) {
                let center = stage.get_center(neuron);
                centers.push(center.map(f64::to_bits));
                let position = net.neuron_position(1, index).unwrap();
                for axis in 0..3 {
                    assert!((center[axis] - position[axis]).abs() < 1e-6);
                }
                let incoming = binding.synapse(0, 0, index).unwrap();
                let outgoing = binding.synapse(1, index, 0).unwrap();
                for endpoint in [
                    stage.get_end(incoming).unwrap(),
                    stage.get_start(outgoing).unwrap(),
                ] {
                    for axis in 0..3 {
                        assert!((endpoint[axis] - center[axis]).abs() < 1e-6);
                    }
                }
            }
        }
        centers.sort_unstable();
        centers.dedup();
        assert_eq!(centers.len(), cap, "displayed neurons must never overlap");
        assert_eq!(binding.neuron(1, 31), None);
        assert_eq!(binding.synapse(1, 31, 0), None);
        assert_eq!(binding.neuron(3, 0), None);

        // The tail is an actual source neuron, not a repaint of the first
        // displayed indices. Its weight and activation must reach its handles.
        let tail = *expected.last().unwrap();
        let last = binding.neuron(1, tail).unwrap();
        let edge = binding.synapse(1, tail, 0).unwrap();
        net.set_neuron_activation(1, tail, 1.0).unwrap();
        net.set_synapse_weight(1, tail, 0, -1.0).unwrap();
        binding.sync_values(&mut stage, &net).unwrap();
        assert_eq!(stage.get_fill_color(last), Some(stored_color(YELLOW)));
        assert_eq!(stage.get_stroke_color(edge), Some(stored_color(RED)));
    }
}

#[test]
fn invalid_layouts_and_excessive_model_allocations_refuse_before_building() {
    for cap in [0, 1] {
        let net = NeuralNetworkMobject::from_layer_sizes(&[2, 31, 2])
            .unwrap()
            .with_config(NeuralNetworkConfig {
                max_displayed_neurons: cap,
                ..NeuralNetworkConfig::default()
            });
        let mut stage = Stage::new();
        let before = stage.snapshot().to_bytes().unwrap();
        assert!(net.add_to(&mut stage).is_err());
        assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
    }

    // These shapes formerly attempted tens of GiB or overflowing vectors
    // before visible-layer elision could impose any geometry budget.
    assert!(NeuralNetworkMobject::from_layer_sizes(&[65_536, 65_536]).is_err());
    assert!(NeuralNetworkMobject::from_layer_sizes(&[usize::MAX]).is_err());
    assert!(NeuralNetworkMobject::introspect_mlp(&[usize::MAX], &[], &[]).is_err());
    assert!(NeuralNetworkMobject::introspect_conv_stack(usize::MAX, &[], &[1]).is_err());

    let too_many_parts = NeuralNetworkMobject::from_layer_sizes(&[256, 256])
        .unwrap()
        .with_config(NeuralNetworkConfig {
            max_displayed_neurons: 256,
            ..NeuralNetworkConfig::default()
        });
    let mut stage = Stage::new();
    let before = stage.snapshot().to_bytes().unwrap();
    assert!(too_many_parts.add_to(&mut stage).is_err());
    assert_eq!(stage.snapshot().to_bytes().unwrap(), before);

    for config in [
        NeuralNetworkConfig {
            layer_spacing: f64::MAX,
            ..NeuralNetworkConfig::default()
        },
        NeuralNetworkConfig {
            neuron_radius: f64::from(f32::MAX),
            ..NeuralNetworkConfig::default()
        },
    ] {
        let net = NeuralNetworkMobject::from_layer_sizes(&[1, 1])
            .unwrap()
            .with_config(config);
        assert!(net.add_to(&mut stage).is_err());
        assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
    }
}

#[test]
fn invalid_live_value_updates_are_atomic() {
    for invalid in 0..5 {
        let mut stage = Stage::new();
        let mut net = NeuralNetworkMobject::from_layer_sizes(&[2, 3, 1]).unwrap();
        let mut binding = net.add_to(&mut stage).unwrap();
        let root = binding.mobject();
        stage.add_to_scene(root).unwrap();
        // A valid first change must not publish before a later refusal.
        net.set_neuron_activation(0, 0, 1.0).unwrap();
        match invalid {
            0 => net.layer_mut(2).unwrap().activations.clear(),
            1 => net.layer_mut(2).unwrap().activations[0] = f64::NAN,
            2 => net.layer_mut(1).unwrap().next_weights[2][0] = f64::INFINITY,
            3 => net.layer_mut(2).unwrap().kind = LayerKind::Output { size: 2 },
            _ => {
                net = net.with_config(NeuralNetworkConfig {
                    layer_spacing: 7.0,
                    ..NeuralNetworkConfig::default()
                });
            }
        }
        let before = stage.snapshot().to_bytes().unwrap();
        let revisions = family_revisions(&stage, root);
        assert!(
            binding.sync_values(&mut stage, &net).is_err(),
            "case {invalid}"
        );
        assert_eq!(
            stage.snapshot().to_bytes().unwrap(),
            before,
            "case {invalid}"
        );
        assert_eq!(family_revisions(&stage, root), revisions, "case {invalid}");
    }
}

#[test]
fn stale_or_modified_bound_families_refuse_before_any_style_write() {
    use fmn_mobject::Mobject;
    use fmn_mobject::record::{RecordBuffer, RecordSchema};

    for invalid in 0..4 {
        let mut stage = Stage::new();
        let mut net = NeuralNetworkMobject::from_layer_sizes(&[2, 3, 1]).unwrap();
        let mut binding = net.add_to(&mut stage).unwrap();
        let root = binding.mobject();
        stage.add_to_scene(root).unwrap();
        let last = binding.neuron(2, 0).unwrap();
        match invalid {
            0 => stage.delete(last).unwrap(),
            1 => {
                stage.detach(root, last);
                let substitute = stage.add(Mobject::new());
                stage.attach(root, substitute).unwrap();
            }
            2 => {
                let child = stage.add(Mobject::new());
                stage.attach(last, child).unwrap();
            }
            _ => {
                let entry = stage.get_mut(last).unwrap();
                entry.buffer =
                    RecordBuffer::new(RecordSchema::mobject(), entry.buffer.len()).unwrap();
            }
        }
        net.set_neuron_activation(0, 0, 1.0).unwrap();
        let before = stage.snapshot().to_bytes().unwrap();
        assert!(
            binding.sync_values(&mut stage, &net).is_err(),
            "case {invalid}"
        );
        assert_eq!(
            stage.snapshot().to_bytes().unwrap(),
            before,
            "case {invalid}"
        );
    }

    let net = NeuralNetworkMobject::from_layer_sizes(&[1, 1]).unwrap();
    let mut original = Stage::new();
    let mut binding = net.add_to(&mut original).unwrap();
    let mut foreign = Stage::new();
    let before = foreign.snapshot().to_bytes().unwrap();
    assert!(binding.sync_values(&mut foreign, &net).is_err());
    assert_eq!(foreign.snapshot().to_bytes().unwrap(), before);
}

#[test]
fn self_golden_locks_canonical_mlp_build() {
    let mut net =
        NeuralNetworkMobject::from_layer_sizes(&[2, 3, 1]).unwrap_or_else(|e| fail(format!("{e}")));

    // Set deterministic activations and weights
    net.set_layer_activations(0, &[0.8, 0.2])
        .unwrap_or_else(|e| fail(format!("{e}")));
    net.set_layer_activations(1, &[0.5, 0.9, 0.1])
        .unwrap_or_else(|e| fail(format!("{e}")));
    net.set_layer_activations(2, &[0.95])
        .unwrap_or_else(|e| fail(format!("{e}")));

    let built = net.build().unwrap_or_else(|e| fail(format!("{e}")));

    let mut bytes = Vec::new();
    let mut stack = vec![&built];
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
        "7b57eef56d27a805df6aebda3db30537e29a2504001b6128c7dd88e3622ff22f",
        "canonical neural network build drifted"
    );
}
