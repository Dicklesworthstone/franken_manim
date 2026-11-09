//! Native neural-network visualization (§2.6, fm-74z).
//!
//! [`NeuralNetworkMobject`] consumes explicit layer, activation and weight
//! values. [`NeuralNetworkBinding`] connects those values to an installed
//! scene family: training-loop updaters can refresh its colors and widths
//! without replacing geometry, handles, placements or other updaters. The
//! caller supplies model values from its training backend. The optional
//! [`NeuralNetworkMobject::feed_forward`] provides an illustrative sigmoid pass.
//!
//! Large layers show their first and last neurons, separated by an elision
//! slot. Their synapses retain the original neuron indices. Ordinary Choreo
//! animations can target the returned neuron and synapse handles; an updater
//! which reads live model values conservatively makes its segment stateful.

use std::fmt;

use fmn_core::color::{Srgb, interpolate_color};
use fmn_core::constants::{BLUE_D, GREY_B, RED, WHITE, YELLOW};
use fmn_core::types::Vec3;
use fmn_mobject::{Mob, RecordBuffer, Stage};

use crate::arc::Dot;
use crate::line::Line;
use crate::style::Style;
use crate::vmobject::{VMobject, v_group};

/// Default horizontal spacing between adjacent layers.
pub const DEFAULT_LAYER_SPACING: f64 = 2.5;
/// Default vertical spacing between adjacent neurons within a layer.
pub const DEFAULT_NEURON_SPACING: f64 = 0.55;
/// Default neuron dot radius.
pub const DEFAULT_NEURON_RADIUS: f64 = 0.14;
/// Default synapse line stroke width.
pub const DEFAULT_SYNAPSE_STROKE_WIDTH: f64 = 1.6;
/// Default threshold for compressing a layer with ellipsis dots.
pub const DEFAULT_MAX_DISPLAYED_NEURONS: usize = 12;
/// Maximum visible neurons, synapses and elision markers in one family.
pub const MAX_NEURAL_NETWORK_PARTS: usize = 65_536;
/// Maximum activation, bias and default-weight values allocated by a recipe.
/// Explicit sparse layer specifications can omit default weight matrices.
pub const MAX_NEURAL_NETWORK_VALUES: usize = 16_777_216;

/// Why a [`NeuralNetworkMobject`] could not be constructed, updated, or built.
#[derive(Debug, Clone, PartialEq)]
pub enum NeuralNetworkError {
    /// The network has zero layers.
    EmptyNetwork,
    /// A layer index was out of bounds.
    InvalidLayerIndex {
        /// Requested layer index.
        index: usize,
        /// Total layer count.
        total_layers: usize,
    },
    /// A neuron index was out of bounds for the given layer.
    InvalidNeuronIndex {
        /// Layer index.
        layer: usize,
        /// Requested neuron index.
        index: usize,
        /// Total neurons in that layer.
        total_neurons: usize,
    },
    /// Vector or matrix dimensions did not match expected shape.
    DimensionMismatch {
        /// Expected dimension.
        expected: usize,
        /// Actual provided dimension.
        got: usize,
        /// Context description.
        context: String,
    },
    /// A geometry construction failed.
    Geometry(String),
    /// A layout control or visible model value is non-finite or invalid.
    InvalidValue(&'static str),
    /// The visible family exceeds its declared allocation budget.
    TooManyParts { requested: usize, max: usize },
    /// A constructor's model-value storage exceeds its declared budget.
    TooManyValues { requested: usize, max: usize },
    /// A live binding's root or one of its original children was deleted.
    StaleBinding(Mob),
    /// Family topology, record schema, layer sizes or configuration changed.
    BindingMismatch(&'static str),
}

impl fmt::Display for NeuralNetworkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyNetwork => write!(f, "neural network must have at least one layer"),
            Self::InvalidLayerIndex {
                index,
                total_layers,
            } => {
                write!(
                    f,
                    "invalid layer index {index}; network has {total_layers} layers"
                )
            }
            Self::InvalidNeuronIndex {
                layer,
                index,
                total_neurons,
            } => {
                write!(
                    f,
                    "invalid neuron index {index} in layer {layer}; layer has {total_neurons} neurons"
                )
            }
            Self::DimensionMismatch {
                expected,
                got,
                context,
            } => {
                write!(
                    f,
                    "dimension mismatch for {context}: expected {expected}, got {got}"
                )
            }
            Self::Geometry(detail) => write!(f, "geometry construction failed: {detail}"),
            Self::InvalidValue(detail) => write!(f, "invalid neural network value: {detail}"),
            Self::TooManyParts { requested, max } => {
                write!(
                    f,
                    "neural network needs {requested} visible parts, above the {max} cap"
                )
            }
            Self::TooManyValues { requested, max } => {
                write!(
                    f,
                    "neural network needs {requested} stored values, above the {max} cap"
                )
            }
            Self::StaleBinding(mob) => write!(f, "neural network binding has stale handle {mob:?}"),
            Self::BindingMismatch(detail) => write!(f, "neural network binding changed: {detail}"),
        }
    }
}

impl std::error::Error for NeuralNetworkError {}

/// The architectural kind of a layer.
#[derive(Debug, Clone, PartialEq)]
pub enum LayerKind {
    /// Input layer.
    Input {
        /// Number of input features.
        size: usize,
    },
    /// Fully-connected dense linear layer.
    Dense {
        /// Number of input features.
        in_features: usize,
        /// Number of output features.
        out_features: usize,
    },
    /// 2D Convolutional layer.
    Conv2d {
        /// Number of input channels.
        in_channels: usize,
        /// Number of output channels.
        out_channels: usize,
        /// Kernel (height, width).
        kernel_size: (usize, usize),
    },
    /// Output layer.
    Output {
        /// Number of output classes/units.
        size: usize,
    },
    /// Custom / other named layer.
    Custom {
        /// Layer name.
        name: String,
        /// Unit count.
        units: usize,
    },
}

impl LayerKind {
    /// Number of units in this layer.
    #[must_use]
    pub fn unit_count(&self) -> usize {
        match self {
            Self::Input { size } | Self::Output { size } => *size,
            Self::Dense { out_features, .. } => *out_features,
            Self::Conv2d { out_channels, .. } => *out_channels,
            Self::Custom { units, .. } => *units,
        }
    }
}

/// Specification of a single layer within [`NeuralNetworkMobject`].
#[derive(Debug, Clone, PartialEq)]
pub struct LayerSpec {
    /// Layer kind and parameters.
    pub kind: LayerKind,
    /// Current activation levels for each neuron in this layer (0.0 to 1.0).
    pub activations: Vec<f64>,
    /// Biases for each neuron in this layer.
    pub biases: Vec<f64>,
    /// Synapse weights to the next layer `[from_neuron][to_neuron]`.
    pub next_weights: Vec<Vec<f64>>,
    /// Optional display label for the layer (e.g. "Conv1", "Hidden", "Softmax").
    pub label: Option<String>,
}

impl LayerSpec {
    /// Create a new layer specification with the given kind.
    #[must_use]
    pub fn new(kind: LayerKind) -> Self {
        let count = kind.unit_count();
        Self {
            kind,
            activations: vec![0.0; count],
            biases: vec![0.0; count],
            next_weights: Vec::new(),
            label: None,
        }
    }

    /// Attach a label.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Unit count of this layer.
    #[must_use]
    pub fn unit_count(&self) -> usize {
        self.kind.unit_count()
    }
}

/// Visual styling configuration for [`NeuralNetworkMobject`].
#[derive(Debug, Clone, PartialEq)]
pub struct NeuralNetworkConfig {
    /// Horizontal spacing between consecutive layers.
    pub layer_spacing: f64,
    /// Vertical spacing between adjacent neurons within a layer.
    pub neuron_spacing: f64,
    /// Radius of neuron dots.
    pub neuron_radius: f64,
    /// Maximum number of neurons to draw before inserting ellipsis dots.
    pub max_displayed_neurons: usize,
    /// Base inactive neuron color.
    pub neuron_base_color: Srgb,
    /// Active neuron fill color when activation = 1.0.
    pub neuron_active_color: Srgb,
    /// Color for positive synapse weights.
    pub positive_weight_color: Srgb,
    /// Color for negative synapse weights.
    pub negative_weight_color: Srgb,
    /// Synapse line stroke width.
    pub synapse_stroke_width: f64,
    /// Base synapse line opacity.
    pub synapse_opacity: f64,
}

impl Default for NeuralNetworkConfig {
    fn default() -> Self {
        Self {
            layer_spacing: DEFAULT_LAYER_SPACING,
            neuron_spacing: DEFAULT_NEURON_SPACING,
            neuron_radius: DEFAULT_NEURON_RADIUS,
            max_displayed_neurons: DEFAULT_MAX_DISPLAYED_NEURONS,
            neuron_base_color: GREY_B,
            neuron_active_color: YELLOW,
            positive_weight_color: BLUE_D,
            negative_weight_color: RED,
            synapse_stroke_width: DEFAULT_SYNAPSE_STROKE_WIDTH,
            synapse_opacity: 0.65,
        }
    }
}

/// A neural-network visualization recipe with explicit model values.
#[derive(Debug, Clone, PartialEq)]
pub struct NeuralNetworkMobject {
    layers: Vec<LayerSpec>,
    config: NeuralNetworkConfig,
}

impl NeuralNetworkMobject {
    /// Construct a multi-layer perceptron (MLP) from layer unit sizes (e.g. `[3, 5, 2]`).
    pub fn from_layer_sizes(sizes: &[usize]) -> Result<Self, NeuralNetworkError> {
        if sizes.is_empty() {
            return Err(NeuralNetworkError::EmptyNetwork);
        }
        let mut values = 0_usize;
        for (index, &size) in sizes.iter().enumerate() {
            values = values.saturating_add(size.saturating_mul(2));
            if let Some(&next) = sizes.get(index + 1) {
                values = values.saturating_add(size.saturating_mul(next));
            }
        }
        admit_values(values)?;
        let mut layers = Vec::with_capacity(sizes.len());
        for (i, &size) in sizes.iter().enumerate() {
            let kind = if i == 0 {
                LayerKind::Input { size }
            } else if i == sizes.len() - 1 {
                LayerKind::Output { size }
            } else {
                LayerKind::Dense {
                    in_features: sizes[i - 1],
                    out_features: size,
                }
            };
            let mut layer = LayerSpec::new(kind);
            if i + 1 < sizes.len() {
                let next_size = sizes[i + 1];
                // Initialize default uniform weights
                layer.next_weights = vec![vec![0.5; next_size]; size];
            }
            layers.push(layer);
        }
        Ok(Self {
            layers,
            config: NeuralNetworkConfig::default(),
        })
    }

    /// Construct a network from explicit [`LayerSpec`] specifications.
    pub fn from_specs(layers: Vec<LayerSpec>) -> Result<Self, NeuralNetworkError> {
        if layers.is_empty() {
            return Err(NeuralNetworkError::EmptyNetwork);
        }
        Ok(Self {
            layers,
            config: NeuralNetworkConfig::default(),
        })
    }

    /// Introspect an MLP with explicit weights and biases.
    pub fn introspect_mlp(
        layer_sizes: &[usize],
        weights: &[Vec<Vec<f64>>],
        biases: &[Vec<f64>],
    ) -> Result<Self, NeuralNetworkError> {
        if layer_sizes.is_empty() {
            return Err(NeuralNetworkError::EmptyNetwork);
        }
        let n_layers = layer_sizes.len();
        if weights.len() + 1 != n_layers {
            return Err(NeuralNetworkError::DimensionMismatch {
                expected: n_layers.saturating_sub(1),
                got: weights.len(),
                context: "weights layer count".into(),
            });
        }
        if biases.len() + 1 != n_layers {
            return Err(NeuralNetworkError::DimensionMismatch {
                expected: n_layers.saturating_sub(1),
                got: biases.len(),
                context: "biases layer count".into(),
            });
        }
        let values = layer_sizes.iter().fold(0_usize, |sum, &size| {
            sum.saturating_add(size.saturating_mul(2))
        });
        let values = weights
            .iter()
            .flat_map(|matrix| matrix.iter())
            .fold(values, |sum, row| sum.saturating_add(row.len()));
        admit_values(values)?;

        let mut layers = Vec::with_capacity(n_layers);
        for i in 0..n_layers {
            let size = layer_sizes[i];
            let kind = if i == 0 {
                LayerKind::Input { size }
            } else if i == n_layers - 1 {
                LayerKind::Output { size }
            } else {
                LayerKind::Dense {
                    in_features: layer_sizes[i - 1],
                    out_features: size,
                }
            };
            let mut layer = LayerSpec::new(kind);
            if i > 0 {
                let layer_biases = &biases[i - 1];
                if layer_biases.len() != size {
                    return Err(NeuralNetworkError::DimensionMismatch {
                        expected: size,
                        got: layer_biases.len(),
                        context: format!("biases for layer {i}"),
                    });
                }
                layer.biases = layer_biases.clone();
            }
            if i < n_layers - 1 {
                let layer_weights = &weights[i];
                if layer_weights.len() != size {
                    return Err(NeuralNetworkError::DimensionMismatch {
                        expected: size,
                        got: layer_weights.len(),
                        context: format!("weight rows for layer {i}"),
                    });
                }
                let next_size = layer_sizes[i + 1];
                for (from_idx, row) in layer_weights.iter().enumerate() {
                    if row.len() != next_size {
                        return Err(NeuralNetworkError::DimensionMismatch {
                            expected: next_size,
                            got: row.len(),
                            context: format!("weights from neuron {from_idx} in layer {i}"),
                        });
                    }
                }
                layer.next_weights = layer_weights.clone();
            }
            layers.push(layer);
        }
        Ok(Self {
            layers,
            config: NeuralNetworkConfig::default(),
        })
    }

    /// Introspect a Convolutional + Dense stack (e.g. Conv2d -> Conv2d -> Dense -> Output).
    pub fn introspect_conv_stack(
        in_channels: usize,
        conv_layers: &[(usize, (usize, usize))],
        dense_sizes: &[usize],
    ) -> Result<Self, NeuralNetworkError> {
        if conv_layers.is_empty() && dense_sizes.is_empty() {
            return Err(NeuralNetworkError::EmptyNetwork);
        }
        let values = conv_layers
            .iter()
            .map(|&(size, _)| size)
            .chain(dense_sizes.iter().copied())
            .fold(in_channels.saturating_mul(2), |sum, size| {
                sum.saturating_add(size.saturating_mul(2))
            });
        admit_values(values)?;
        let mut layers = Vec::new();
        let mut current_channels = in_channels;

        // Input layer
        layers.push(LayerSpec::new(LayerKind::Input { size: in_channels }).with_label("Input"));

        // Conv layers
        for (i, &(out_ch, k_size)) in conv_layers.iter().enumerate() {
            layers.push(
                LayerSpec::new(LayerKind::Conv2d {
                    in_channels: current_channels,
                    out_channels: out_ch,
                    kernel_size: k_size,
                })
                .with_label(format!("Conv{}", i + 1)),
            );
            current_channels = out_ch;
        }

        // Dense layers
        for (i, &size) in dense_sizes.iter().enumerate() {
            let is_last = i == dense_sizes.len() - 1;
            let kind = if is_last {
                LayerKind::Output { size }
            } else {
                let in_f = if i == 0 {
                    current_channels
                } else {
                    dense_sizes[i - 1]
                };
                LayerKind::Dense {
                    in_features: in_f,
                    out_features: size,
                }
            };
            layers.push(LayerSpec::new(kind).with_label(if is_last { "Output" } else { "Dense" }));
        }

        Ok(Self {
            layers,
            config: NeuralNetworkConfig::default(),
        })
    }

    /// Customize the styling configuration.
    #[must_use]
    pub fn with_config(mut self, config: NeuralNetworkConfig) -> Self {
        self.config = config;
        self
    }

    /// Number of layers in the network.
    #[must_use]
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Access layer specifications.
    #[must_use]
    pub fn layers(&self) -> &[LayerSpec] {
        &self.layers
    }

    /// Get a mutable reference to a layer specification.
    pub fn layer_mut(&mut self, index: usize) -> Result<&mut LayerSpec, NeuralNetworkError> {
        let total = self.layers.len();
        self.layers
            .get_mut(index)
            .ok_or(NeuralNetworkError::InvalidLayerIndex {
                index,
                total_layers: total,
            })
    }

    /// Set activations for all neurons in a specific layer.
    pub fn set_layer_activations(
        &mut self,
        layer_idx: usize,
        activations: &[f64],
    ) -> Result<(), NeuralNetworkError> {
        let layer = self.layer_mut(layer_idx)?;
        let expected = layer.unit_count();
        if activations.len() != expected {
            return Err(NeuralNetworkError::DimensionMismatch {
                expected,
                got: activations.len(),
                context: format!("activations for layer {layer_idx}"),
            });
        }
        if activations.iter().any(|value| !value.is_finite()) {
            return Err(NeuralNetworkError::InvalidValue(
                "activations must be finite",
            ));
        }
        layer.activations = activations.to_vec();
        Ok(())
    }

    /// Set activation for an individual neuron.
    pub fn set_neuron_activation(
        &mut self,
        layer_idx: usize,
        neuron_idx: usize,
        value: f64,
    ) -> Result<(), NeuralNetworkError> {
        if !value.is_finite() {
            return Err(NeuralNetworkError::InvalidValue(
                "activation must be finite",
            ));
        }
        let layer = self.layer_mut(layer_idx)?;
        let total = layer.unit_count();
        let act = layer.activations.get_mut(neuron_idx).ok_or(
            NeuralNetworkError::InvalidNeuronIndex {
                layer: layer_idx,
                index: neuron_idx,
                total_neurons: total,
            },
        )?;
        *act = value;
        Ok(())
    }

    /// Set synapse weight between two neurons.
    pub fn set_synapse_weight(
        &mut self,
        layer_idx: usize,
        from_neuron: usize,
        to_neuron: usize,
        weight: f64,
    ) -> Result<(), NeuralNetworkError> {
        if !weight.is_finite() {
            return Err(NeuralNetworkError::InvalidValue("weight must be finite"));
        }
        let layer = self.layer_mut(layer_idx)?;
        let unit_count = layer.unit_count();
        let row = layer.next_weights.get_mut(from_neuron).ok_or(
            NeuralNetworkError::InvalidNeuronIndex {
                layer: layer_idx,
                index: from_neuron,
                total_neurons: unit_count,
            },
        )?;
        let total_next = row.len();
        let cell = row
            .get_mut(to_neuron)
            .ok_or(NeuralNetworkError::InvalidNeuronIndex {
                layer: layer_idx + 1,
                index: to_neuron,
                total_neurons: total_next,
            })?;
        *cell = weight;
        Ok(())
    }

    /// Run a forward feedforward pass with logistic sigmoid activation, updating
    /// internal layer activations and returning the final output vector.
    pub fn feed_forward(&mut self, input: &[f64]) -> Result<Vec<f64>, NeuralNetworkError> {
        self.set_layer_activations(0, input)?;
        let mut current = input.to_vec();

        for i in 0..self.layers.len() - 1 {
            let next_size = self.layers[i + 1].unit_count();
            let mut next_act = vec![0.0; next_size];
            let biases = &self.layers[i + 1].biases;
            let weights = &self.layers[i].next_weights;

            for to_idx in 0..next_size {
                let mut z = if to_idx < biases.len() {
                    biases[to_idx]
                } else {
                    0.0
                };
                for (from_idx, &val) in current.iter().enumerate() {
                    if let Some(&w) = weights.get(from_idx).and_then(|row| row.get(to_idx)) {
                        z += val * w;
                    }
                }
                // Sigmoid activation: 1 / (1 + e^(-z))
                let sigmoid = 1.0 / (1.0 + fmn_dmath::exp(-z));
                next_act[to_idx] = sigmoid;
            }
            self.layers[i + 1].activations = next_act.clone();
            current = next_act;
        }
        Ok(current)
    }

    /// Calculate a displayed neuron's position in the detached layout.
    /// Elided neurons and invalid indices have no individual position.
    #[must_use]
    pub fn neuron_position(&self, layer_idx: usize, neuron_idx: usize) -> Option<Vec3> {
        if layer_idx >= self.layers.len() {
            return None;
        }
        let total_layers = self.layers.len();
        let layer_unit_count = self.layers[layer_idx].unit_count();
        if neuron_idx >= layer_unit_count {
            return None;
        }

        // Center layers horizontally about the origin
        let x = (layer_idx as f64 - (total_layers as f64 - 1.0) / 2.0) * self.config.layer_spacing;

        // One empty slot separates the visible first/last neurons. It is also
        // the ellipsis center, including for odd display caps.
        let displayed_count = layer_unit_count.min(self.config.max_displayed_neurons);
        let (effective_idx, slots) = if layer_unit_count > displayed_count {
            let leading = displayed_count.div_ceil(2);
            let trailing = displayed_count / 2;
            if neuron_idx < leading {
                (neuron_idx, displayed_count + 1)
            } else if neuron_idx >= layer_unit_count - trailing {
                (
                    leading + 1 + (neuron_idx - (layer_unit_count - trailing)),
                    displayed_count + 1,
                )
            } else {
                return None;
            }
        } else {
            (neuron_idx, displayed_count)
        };

        let y = ((slots as f64 - 1.0) / 2.0 - effective_idx as f64) * self.config.neuron_spacing;
        Some([x, y, 0.0])
    }

    fn visible_neurons(&self, layer: usize) -> impl Iterator<Item = usize> {
        let count = self.layers[layer].unit_count();
        let displayed = count.min(self.config.max_displayed_neurons);
        let leading = if count == displayed {
            count
        } else {
            displayed.div_ceil(2)
        };
        let trailing = if count == displayed { 0 } else { displayed / 2 };
        (0..leading).chain(count - trailing..count)
    }

    fn validate(&self) -> Result<(), NeuralNetworkError> {
        if self.layers.is_empty() {
            return Err(NeuralNetworkError::EmptyNetwork);
        }
        for value in [
            self.config.layer_spacing,
            self.config.neuron_spacing,
            self.config.neuron_radius,
            self.config.synapse_stroke_width,
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(NeuralNetworkError::InvalidValue(
                    "spacing, radius and stroke width must be positive and finite",
                ));
            }
        }
        if self.config.max_displayed_neurons < 2 {
            return Err(NeuralNetworkError::InvalidValue(
                "max_displayed_neurons must be at least two",
            ));
        }
        if !self.config.synapse_opacity.is_finite()
            || !(0.0..=1.0).contains(&self.config.synapse_opacity)
        {
            return Err(NeuralNetworkError::InvalidValue(
                "synapse opacity must be in [0, 1]",
            ));
        }
        for color in [
            self.config.neuron_base_color,
            self.config.neuron_active_color,
            self.config.positive_weight_color,
            self.config.negative_weight_color,
        ] {
            if [color.r, color.g, color.b]
                .iter()
                .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
            {
                return Err(NeuralNetworkError::InvalidValue(
                    "colors must have finite components in [0, 1]",
                ));
            }
        }
        let mut parts = 0_usize;
        for (index, layer) in self.layers.iter().enumerate() {
            let count = layer.unit_count();
            if count == 0 || layer.activations.len() != count || layer.biases.len() != count {
                return Err(NeuralNetworkError::InvalidValue(
                    "layers must have nonzero size and matching activation/bias vectors",
                ));
            }
            if !layer.next_weights.is_empty() {
                let next = self.layers.get(index + 1).map_or(0, LayerSpec::unit_count);
                if layer.next_weights.len() != count
                    || layer.next_weights.iter().any(|row| row.len() != next)
                {
                    return Err(NeuralNetworkError::InvalidValue(
                        "weight matrix dimensions must match adjacent layers",
                    ));
                }
            }
            let visible = count.min(self.config.max_displayed_neurons);
            let next_visible = self.layers.get(index + 1).map_or(0, |next| {
                next.unit_count().min(self.config.max_displayed_neurons)
            });
            parts = parts
                .saturating_add(visible)
                .saturating_add(visible.saturating_mul(next_visible));
            if count > visible {
                parts = parts.saturating_add(3);
            }
            if parts > MAX_NEURAL_NETWORK_PARTS {
                return Err(NeuralNetworkError::TooManyParts {
                    requested: parts,
                    max: MAX_NEURAL_NETWORK_PARTS,
                });
            }
            for neuron in self.visible_neurons(index) {
                self.neuron_color(index, neuron)?;
                let point =
                    self.neuron_position(index, neuron)
                        .ok_or(NeuralNetworkError::InvalidValue(
                            "displayed neuron has no position",
                        ))?;
                if point.iter().any(|value| {
                    !value.is_finite()
                        || value.abs() + 2.0 * self.config.neuron_radius > f64::from(f32::MAX)
                }) {
                    return Err(NeuralNetworkError::InvalidValue(
                        "neuron geometry must fit finite f32 records",
                    ));
                }
                if index + 1 < self.layers.len() {
                    for next in self.visible_neurons(index + 1) {
                        self.synapse_style(index, neuron, next)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn neuron_color(&self, layer: usize, neuron: usize) -> Result<Srgb, NeuralNetworkError> {
        let value =
            *self.layers[layer]
                .activations
                .get(neuron)
                .ok_or(NeuralNetworkError::InvalidValue(
                    "missing displayed neuron activation",
                ))?;
        if !value.is_finite() {
            return Err(NeuralNetworkError::InvalidValue(
                "displayed activations must be finite",
            ));
        }
        Ok(interpolate_color(
            self.config.neuron_base_color,
            self.config.neuron_active_color,
            value.clamp(0.0, 1.0),
        ))
    }

    fn synapse_style(
        &self,
        layer: usize,
        from: usize,
        to: usize,
    ) -> Result<Style, NeuralNetworkError> {
        let weights = &self.layers[layer].next_weights;
        let weight = if weights.is_empty() {
            0.5
        } else {
            *weights.get(from).and_then(|row| row.get(to)).ok_or(
                NeuralNetworkError::InvalidValue("missing displayed synapse weight"),
            )?
        };
        if !weight.is_finite() {
            return Err(NeuralNetworkError::InvalidValue(
                "displayed weights must be finite",
            ));
        }
        let color = if weight >= 0.0 {
            self.config.positive_weight_color
        } else {
            self.config.negative_weight_color
        };
        let magnitude = weight.abs().min(1.0);
        let width = (self.config.synapse_stroke_width * (0.4 + 0.6 * magnitude)).clamp(0.5, 6.0);
        let opacity = (self.config.synapse_opacity * (0.3 + 0.7 * magnitude)).clamp(0.1, 1.0);
        Ok(Style::default().stroke(color, width, opacity))
    }

    /// Build the full visual representation into a [`VMobject`] family.
    ///
    /// Direct children are painter-ordered: synapses first, followed by
    /// each layer's displayed neurons and optional elision markers.
    pub fn build(&self) -> Result<VMobject, NeuralNetworkError> {
        self.validate()?;

        let mut synapse_mobjects = Vec::new();
        let mut neuron_mobjects = Vec::new();

        // 1. Build Synapses (drawn underneath neurons)
        for layer_idx in 0..self.layers.len() {
            if layer_idx + 1 >= self.layers.len() {
                continue;
            }
            for from_idx in self.visible_neurons(layer_idx) {
                let from_pos = self
                    .neuron_position(layer_idx, from_idx)
                    .ok_or_else(|| NeuralNetworkError::Geometry("missing from_pos".into()))?;

                for to_idx in self.visible_neurons(layer_idx + 1) {
                    let to_pos = self
                        .neuron_position(layer_idx + 1, to_idx)
                        .ok_or_else(|| NeuralNetworkError::Geometry("missing to_pos".into()))?;

                    let style = self.synapse_style(layer_idx, from_idx, to_idx)?;
                    let line = Line::new(from_pos, to_pos)
                        .color(style.stroke_color)
                        .style(style);
                    let built_line = line
                        .build()
                        .map_err(|e| NeuralNetworkError::Geometry(format!("{e}")))?;
                    synapse_mobjects.push(built_line);
                }
            }
        }

        // 2. Build Neurons (drawn on top of synapses)
        for (layer_idx, layer) in self.layers.iter().enumerate() {
            let unit_count = layer.unit_count();
            let is_compressed = unit_count > self.config.max_displayed_neurons;
            for neuron_idx in self.visible_neurons(layer_idx) {
                let pos = self
                    .neuron_position(layer_idx, neuron_idx)
                    .ok_or_else(|| NeuralNetworkError::Geometry("missing neuron_pos".into()))?;

                let color = self.neuron_color(layer_idx, neuron_idx)?;

                let dot = Dot::new()
                    .point(pos)
                    .radius(self.config.neuron_radius)
                    .color(color)
                    .style(Style::default().fill(color, 1.0).stroke(WHITE, 1.0, 1.0));
                neuron_mobjects.push(dot.build());
            }

            // If compressed, add ellipsis indicator dots
            if is_compressed {
                let mid_x = (layer_idx as f64 - (self.layers.len() as f64 - 1.0) / 2.0)
                    * self.config.layer_spacing;
                let gap_y = (self.config.max_displayed_neurons as f64 / 2.0
                    - self.config.max_displayed_neurons.div_ceil(2) as f64)
                    * self.config.neuron_spacing;
                for offset_y in [-0.25, 0.0, 0.25] {
                    let el_dot = Dot::new()
                        .point([mid_x, gap_y + offset_y * self.config.neuron_spacing, 0.0])
                        .radius(self.config.neuron_radius * 0.35)
                        .color(self.config.neuron_base_color)
                        .style(Style::default().fill(self.config.neuron_base_color, 0.8));
                    neuron_mobjects.push(el_dot.build());
                }
            }
        }

        let mut all_children = synapse_mobjects;
        all_children.extend(neuron_mobjects);
        Ok(v_group(all_children))
    }

    /// Install this visual family and retain stable displayed-neuron and
    /// synapse identities. Add the returned root to the scene separately.
    ///
    /// # Errors
    /// Invalid layout/model values and the visible-family resource budget are
    /// checked before adding any records to the arena.
    pub fn add_to(&self, stage: &mut Stage) -> Result<NeuralNetworkBinding, NeuralNetworkError> {
        let root = stage.add(self.build()?);
        let members = stage
            .get(root)
            .expect("new neural family is live")
            .submobjects()
            .to_vec();
        let mut synapses = Vec::new();
        let mut neurons = Vec::new();
        let mut child = 0;
        for layer in 0..self.layers.len().saturating_sub(1) {
            for from in self.visible_neurons(layer) {
                for to in self.visible_neurons(layer + 1) {
                    synapses.push((layer, from, to, members[child]));
                    child += 1;
                }
            }
        }
        for layer in 0..self.layers.len() {
            for neuron in self.visible_neurons(layer) {
                neurons.push((layer, neuron, members[child]));
                child += 1;
            }
            if self.layers[layer].unit_count() > self.config.max_displayed_neurons {
                child += 3;
            }
        }
        debug_assert_eq!(child, members.len());
        let children: Vec<_> = members
            .into_iter()
            .map(|mob| {
                (
                    mob,
                    stage
                        .get(mob)
                        .expect("new neural child is live")
                        .buffer
                        .len(),
                )
            })
            .collect();
        let scratch_capacity = children.iter().map(|(_, rows)| rows * 4).max().unwrap_or(0);
        Ok(NeuralNetworkBinding {
            root,
            children,
            neurons,
            synapses,
            sizes: self.layers.iter().map(LayerSpec::unit_count).collect(),
            config: self.config.clone(),
            scratch: Vec::with_capacity(scratch_capacity),
        })
    }
}

fn admit_values(requested: usize) -> Result<(), NeuralNetworkError> {
    if requested > MAX_NEURAL_NETWORK_VALUES {
        Err(NeuralNetworkError::TooManyValues {
            requested,
            max: MAX_NEURAL_NETWORK_VALUES,
        })
    } else {
        Ok(())
    }
}

/// An installed neural family whose displayed objects retain their identities.
///
/// [`Self::sync_values`] updates neuron fill RGB and synapse stroke RGB, alpha
/// and width. It preserves placements, geometry, neuron opacity, other styles,
/// updaters and scene membership. Values in elided neurons do not require any
/// render work. Refresh costs are proportional to the displayed family, not
/// the full model's dense weight matrix.
///
/// Layout/configuration changes require a new binding. Deleting, regrouping,
/// reordering or changing the record shape of a bound child invalidates the
/// binding, and refresh refuses before writing any style. Removing the root
/// from the scene and later readding it is supported. Clones refer to the same
/// arena handles; they do not bind copies of the visual family.
///
/// ```
/// use fmn_library::NeuralNetworkMobject;
/// use fmn_mobject::Stage;
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut stage = Stage::new();
/// let mut model = NeuralNetworkMobject::from_layer_sizes(&[2, 4, 1])?;
/// let mut live = model.add_to(&mut stage)?;
/// stage.add_to_scene(live.mobject())?;
/// model.set_layer_activations(1, &[0.2, 0.5, 0.8, 1.0])?;
/// model.set_synapse_weight(0, 0, 3, -0.9)?;
/// live.sync_values(&mut stage, &model)?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct NeuralNetworkBinding {
    root: Mob,
    children: Vec<(Mob, usize)>,
    neurons: Vec<(usize, usize, Mob)>,
    synapses: Vec<(usize, usize, usize, Mob)>,
    sizes: Vec<usize>,
    config: NeuralNetworkConfig,
    scratch: Vec<f32>,
}

impl NeuralNetworkBinding {
    /// The installed family root for scene membership or whole-family motion.
    #[must_use]
    pub fn mobject(&self) -> Mob {
        self.root
    }

    /// A displayed neuron, addressed by its original model index.
    /// Elided and out-of-range neurons return `None`.
    #[must_use]
    pub fn neuron(&self, layer: usize, index: usize) -> Option<Mob> {
        self.neurons
            .iter()
            .find_map(|&(l, n, mob)| (l == layer && n == index).then_some(mob))
    }

    /// A displayed connection from `layer` to `layer + 1`, using model indices.
    #[must_use]
    pub fn synapse(&self, layer: usize, from: usize, to: usize) -> Option<Mob> {
        self.synapses
            .iter()
            .find_map(|&(l, a, b, mob)| (l == layer && a == from && b == to).then_some(mob))
    }

    fn validate(
        &self,
        stage: &Stage,
        model: &NeuralNetworkMobject,
    ) -> Result<(), NeuralNetworkError> {
        if model.config != self.config || model.layers.len() != self.sizes.len() {
            return Err(NeuralNetworkError::BindingMismatch(
                "layout configuration or layer count",
            ));
        }
        for (layer, &size) in model.layers.iter().zip(&self.sizes) {
            if layer.unit_count() != size
                || layer.activations.len() != size
                || layer.biases.len() != size
                || (!layer.next_weights.is_empty() && layer.next_weights.len() != size)
            {
                return Err(NeuralNetworkError::BindingMismatch("layer dimensions"));
            }
        }
        let root = stage
            .get(self.root)
            .ok_or(NeuralNetworkError::StaleBinding(self.root))?;
        if !root.buffer.is_empty()
            || root.submobjects().len() != self.children.len()
            || root
                .submobjects()
                .iter()
                .zip(&self.children)
                .any(|(&actual, &(expected, _))| actual != expected)
        {
            return Err(NeuralNetworkError::BindingMismatch(
                "family membership or order",
            ));
        }
        for &(mob, rows) in &self.children {
            let entry = stage
                .get(mob)
                .ok_or(NeuralNetworkError::StaleBinding(mob))?;
            if !entry.submobjects().is_empty()
                || entry.buffer.len() != rows
                || entry.buffer.schema().field_width("point") != Some(3)
                || entry.buffer.schema().field_width("fill_rgba") != Some(4)
                || entry.buffer.schema().field_width("stroke_rgba") != Some(4)
                || entry.buffer.schema().field_width("stroke_width") != Some(1)
            {
                return Err(NeuralNetworkError::BindingMismatch(
                    "child geometry or record schema",
                ));
            }
        }
        // Validate every value before the first write, including the last
        // displayed synapse. No model callbacks or arena mutation occur here.
        for &(layer, neuron, _) in &self.neurons {
            model.neuron_color(layer, neuron)?;
        }
        for &(layer, from, to, _) in &self.synapses {
            let weights = &model.layers[layer].next_weights;
            if !weights.is_empty() && weights[from].len() != self.sizes[layer + 1] {
                return Err(NeuralNetworkError::BindingMismatch("weight row dimensions"));
            }
            model.synapse_style(layer, from, to)?;
        }
        Ok(())
    }

    /// Refresh the installed visual values without rebuilding any geometry.
    ///
    /// Reuses column scratch and leaves unchanged columns' revisions intact.
    /// This method reads live records, so external styling and writable views
    /// cannot make a cached model value suppress a necessary update.
    ///
    /// # Errors
    /// Stale/restructured families, changed model layout, malformed dimensions
    /// and non-finite displayed values. Every such refusal precedes all writes.
    pub fn sync_values(
        &mut self,
        stage: &mut Stage,
        model: &NeuralNetworkMobject,
    ) -> Result<(), NeuralNetworkError> {
        self.validate(stage, model)?;
        for &(layer, neuron, mob) in &self.neurons {
            let color = model.neuron_color(layer, neuron)?;
            let buffer = &mut stage.get_mut(mob).expect("binding was validated").buffer;
            sync_rgb(buffer, "fill_rgba", color, None, &mut self.scratch);
        }
        for &(layer, from, to, mob) in &self.synapses {
            let style = model.synapse_style(layer, from, to)?;
            let buffer = &mut stage.get_mut(mob).expect("binding was validated").buffer;
            sync_rgb(
                buffer,
                "stroke_rgba",
                style.stroke_color,
                Some(style.stroke_opacity),
                &mut self.scratch,
            );
            buffer.read_column_into("stroke_width", &mut self.scratch);
            let width = style.stroke_width as f32;
            if self.scratch.iter().any(|&value| value != width) {
                self.scratch.fill(width);
                buffer.write_range("stroke_width", 0, &self.scratch);
            }
        }
        Ok(())
    }
}

fn sync_rgb(
    buffer: &mut RecordBuffer,
    field: &str,
    color: Srgb,
    opacity: Option<f64>,
    scratch: &mut Vec<f32>,
) {
    buffer.read_column_into(field, scratch);
    let rgb = [color.r as f32, color.g as f32, color.b as f32];
    let alpha = opacity.map(|value| value as f32);
    let mut changed = false;
    for rgba in scratch.chunks_exact_mut(4) {
        if rgba[..3] != rgb || alpha.is_some_and(|value| rgba[3] != value) {
            rgba[..3].copy_from_slice(&rgb);
            if let Some(alpha) = alpha {
                rgba[3] = alpha;
            }
            changed = true;
        }
    }
    if changed {
        buffer.write_range(field, 0, scratch);
    }
}
