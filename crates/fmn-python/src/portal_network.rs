//! Bounded host entry to Atlas's network layouts and animation schedules.
//! No Python graph algorithm, external networkx installation, or scene mutation.
use std::collections::BTreeSet;

use fmn_library::{GraphLayout, GraphTraversal, NetworkGraph};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyList, PyString, PyTuple};

const MAX_NODES: usize = 1024;
const MAX_EDGES: usize = 8192;
const MAX_LABEL_BYTES: usize = 1_048_576;
const MAX_SPRING_WORK: usize = 16_777_216;

type LayoutRows = (Vec<String>, Vec<(String, String)>, Vec<[f64; 3]>);

struct GraphInput {
    names: Vec<String>,
    pairs: Vec<(String, String)>,
    label_bytes: usize,
}

impl GraphInput {
    fn network(&self) -> NetworkGraph {
        let names: Vec<_> = self.names.iter().map(String::as_str).collect();
        let pairs: Vec<_> = self
            .pairs
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        NetworkGraph::from_edge_list(&names, &pairs)
    }
}

fn label(value: &Bound<'_, PyAny>, bytes: &mut usize) -> PyResult<String> {
    let text = value.cast::<PyString>()?.to_str()?;
    *bytes = bytes
        .checked_add(text.len())
        .filter(|&n| n <= MAX_LABEL_BYTES)
        .ok_or_else(|| PyValueError::new_err("network labels exceed the 1 MiB budget"))?;
    Ok(text.to_owned())
}

fn graph_inputs(nodes: &Bound<'_, PyList>, edges: &Bound<'_, PyList>) -> PyResult<GraphInput> {
    if nodes.len() > MAX_NODES || edges.len() > MAX_EDGES {
        return Err(PyValueError::new_err(
            "network exceeds 1024 nodes or 8192 edges",
        ));
    }
    let mut bytes = 0;
    let mut names = Vec::new();
    let mut known = BTreeSet::new();
    for item in nodes.iter() {
        let name = label(&item, &mut bytes)?;
        if known.insert(name.clone()) {
            names.push(name);
        }
    }
    let mut pairs = Vec::new();
    let mut seen_edges = BTreeSet::new();
    for item in edges.iter() {
        let pair = item.cast::<PyTuple>()?;
        if pair.len() != 2 {
            return Err(PyValueError::new_err("a network edge requires two labels"));
        }
        let a = label(&pair.get_item(0)?, &mut bytes)?;
        let b = label(&pair.get_item(1)?, &mut bytes)?;
        for name in [&a, &b] {
            if known.insert(name.clone()) {
                if names.len() == MAX_NODES {
                    return Err(PyValueError::new_err("network exceeds 1024 nodes"));
                }
                names.push(name.clone());
            }
        }
        let key = if a <= b {
            (a.clone(), b.clone())
        } else {
            (b.clone(), a.clone())
        };
        if seen_edges.insert(key) {
            pairs.push((a, b));
        }
    }
    Ok(GraphInput {
        names,
        pairs,
        label_bytes: bytes,
    })
}

#[pyfunction(signature = (nodes, edges, layout="spring", *, seed=0, iterations=50, ideal_edge=1.0, shells=None, root=None))]
#[allow(clippy::too_many_arguments)] // The host-facing layout option schema.
fn _network_graph_layout(
    py: Python<'_>,
    nodes: &Bound<'_, PyList>,
    edges: &Bound<'_, PyList>,
    layout: &str,
    seed: u64,
    iterations: usize,
    ideal_edge: f64,
    shells: Option<&Bound<'_, PyList>>,
    root: Option<String>,
) -> PyResult<LayoutRows> {
    let input = graph_inputs(nodes, edges)?;
    let mut bytes = input.label_bytes;
    let algorithm = match layout {
        "circular" => GraphLayout::Circular,
        "shell" => {
            let groups =
                shells.ok_or_else(|| PyValueError::new_err("shell layout requires shells"))?;
            if groups.len() > MAX_NODES {
                return Err(PyValueError::new_err("too many network shells"));
            }
            let mut partition = Vec::new();
            let mut count = 0usize;
            for group in groups.iter() {
                let group = group.cast::<PyList>()?;
                count = count
                    .checked_add(group.len())
                    .filter(|&n| n <= MAX_NODES)
                    .ok_or_else(|| PyValueError::new_err("too many shell members"))?;
                let mut ring = Vec::new();
                for item in group.iter() {
                    ring.push(label(&item, &mut bytes)?);
                }
                partition.push(ring);
            }
            GraphLayout::Shell { shells: partition }
        }
        "breadth_first" => GraphLayout::BreadthFirst {
            root: root
                .ok_or_else(|| PyValueError::new_err("breadth_first layout requires root"))?,
        },
        "spring" => {
            let work = input
                .names
                .len()
                .checked_mul(input.names.len().saturating_sub(1))
                .map(|n| n / 2)
                .and_then(|n| n.checked_add(input.pairs.len()))
                .and_then(|n| n.checked_mul(iterations));
            if iterations == 0 || iterations > 1000 || work.is_none_or(|n| n > MAX_SPRING_WORK) {
                return Err(PyValueError::new_err(
                    "spring layout exceeds its bounded iteration work",
                ));
            }
            if !ideal_edge.is_finite() || !(1e-6..=1e6).contains(&ideal_edge) {
                return Err(PyValueError::new_err(
                    "spring ideal_edge must be finite and in [1e-6, 1e6]",
                ));
            }
            GraphLayout::Spring {
                seed,
                iterations,
                ideal_edge,
            }
        }
        _ => return Err(PyValueError::new_err("unknown native network layout")),
    };
    py.detach(move || {
        let graph = input
            .network()
            .laid_out(&algorithm)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let mut points = Vec::with_capacity(input.names.len());
        for name in &input.names {
            let point = graph
                .position(name)
                .filter(|p| p.iter().all(|v| v.is_finite()))
                .ok_or_else(|| {
                    PyValueError::new_err("network layout did not produce a finite position")
                })?;
            points.push(point);
        }
        Ok((input.names, input.pairs, points))
    })
}

#[pyfunction(signature = (nodes, edges, root, order="breadth_first"))]
fn _network_graph_traversal(
    py: Python<'_>,
    nodes: &Bound<'_, PyList>,
    edges: &Bound<'_, PyList>,
    root: &Bound<'_, PyAny>,
    order: &str,
) -> PyResult<Vec<String>> {
    let mut input = graph_inputs(nodes, edges)?;
    let root = label(root, &mut input.label_bytes)?;
    let traversal = match order {
        "breadth_first" => GraphTraversal::BreadthFirst,
        "depth_first" => GraphTraversal::DepthFirst,
        _ => return Err(PyValueError::new_err("unknown native network traversal")),
    };
    py.detach(move || {
        input
            .network()
            .traversal_order(&root, traversal)
            .map_err(|error| PyValueError::new_err(error.to_string()))
    })
}

#[pyfunction]
fn _network_graph_flow_durations(
    py: Python<'_>,
    nodes: &Bound<'_, PyList>,
    edges: &Bound<'_, PyList>,
    steps: &Bound<'_, PyList>,
) -> PyResult<Vec<f64>> {
    if steps.len() > MAX_EDGES {
        return Err(PyValueError::new_err("network flow exceeds 8192 steps"));
    }
    let mut input = graph_inputs(nodes, edges)?;
    let mut owned = Vec::with_capacity(steps.len());
    for step in steps.iter() {
        let row = step.cast::<PyTuple>()?;
        if row.len() != 3 {
            return Err(PyValueError::new_err(
                "a network flow step requires source, destination and weight",
            ));
        }
        let from = label(&row.get_item(0)?, &mut input.label_bytes)?;
        let to = label(&row.get_item(1)?, &mut input.label_bytes)?;
        let weight = row.get_item(2)?.extract::<f64>()?;
        owned.push((from, to, weight));
    }
    py.detach(move || {
        let borrowed: Vec<_> = owned
            .iter()
            .map(|(from, to, weight)| (from.as_str(), to.as_str(), *weight))
            .collect();
        input
            .network()
            .flow_durations(&borrowed)
            .map_err(|error| PyValueError::new_err(error.to_string()))
    })
}

pub(crate) fn install(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(_network_graph_layout, module)?)?;
    module.add_function(wrap_pyfunction!(_network_graph_traversal, module)?)?;
    module.add_function(wrap_pyfunction!(_network_graph_flow_durations, module)?)?;
    Ok(())
}

/// Exercise graph traversal/flow through real embedded Python scenes and PNGs.
/// Returns `(behavioral_cases, rendered_frames, thread_replays)`. The two
/// animation sequences are byte-identical at one, four and sixteen workers;
/// pulse cleanup restores the original graph pixels and authored group hooks
/// retain the exact shared frame clock.
///
/// Embedded modules do not have an installed extension identity, so this
/// runner uses standard PNG output. The same acceptance script defaults to
/// certified output when run against an installed wheel with source bytes.
///
/// # Errors
/// Returns the original Python/native acceptance failure.
#[cfg(any(test, feature = "gauntlet"))]
pub fn run_portal_gauntlet_network_animations(
    root: &std::path::Path,
) -> Result<(u64, u64, u64), String> {
    run_network_animation_acceptance(Some(root))
}

#[cfg(any(test, feature = "gauntlet"))]
fn run_network_animation_acceptance(
    root: Option<&std::path::Path>,
) -> Result<(u64, u64, u64), String> {
    crate::with_python_test_module("native network animations", |py, _module, globals| {
        let code = std::ffi::CString::new(include_str!("../tests/network_graph.py"))
            .map_err(|error| error.to_string())?;
        globals
            .set_item(
                "__file__",
                concat!(env!("CARGO_MANIFEST_DIR"), "/tests/network_graph.py"),
            )
            .map_err(|error| error.to_string())?;
        py.run(code.as_c_str(), Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        globals
            .get_item("run_network_animation_acceptance")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "network animation acceptance entry point is absent".to_owned())?
            .call1((root.map(std::path::Path::to_path_buf), false))
            .inspect_err(|error| error.print(py))
            .and_then(|value| value.extract::<(u64, u64, u64)>())
            .map_err(|error| error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use pyo3::types::{PyAnyMethods, PyDictMethods};

    #[test]
    fn native_network_layouts() {
        crate::with_python_test_module("native network layouts", |py, _module, globals| {
            let code =
                std::ffi::CString::new(include_str!("../tests/native_network_layout.py")).unwrap();
            py.run(code.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .unwrap();
            globals
                .get_item("run_network_layout_acceptance")
                .unwrap()
                .unwrap()
                .call0()
                .inspect_err(|error| error.print(py))
                .unwrap();
        });
    }

    #[test]
    fn native_network_animation_lifecycle_and_pngs() {
        assert_eq!(
            super::run_network_animation_acceptance(None).unwrap(),
            (9, 78, 6)
        );
    }
}
