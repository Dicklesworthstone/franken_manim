//! PG-8 binding-tax measurement harness (§17.2, fm-zoi).
//!
//! This module is the bridge-side producer for the PG-8 class table. It
//! drives the *real* bridge — an embedded CPython interpreter running the
//! production `manimlib` module — through the four declared scene classes,
//! and times full workload repetitions with a monotonic clock. It performs
//! no policy evaluation and no artifact I/O: `fmn-conformance`'s
//! `perf_pg8` module owns identity, statistics, and evidence; this module
//! returns raw per-repetition observations plus the final scene state
//! (the self-golden input proving the measured workload did the declared
//! work).
//!
//! The `native-builtins` class also measures a pure-Rust twin — the same
//! mobjects, the same engine-executable updaters, no interpreter — so the
//! gate's ratio-ppm value is the observed binding tax on a callback-free
//! scene rather than an absolute number on one host.
//!
//! Timing values are observations, not assertions: the harness marks a
//! repetition invalid only on a timer anomaly (zero elapsed), and every
//! repetition is retained for the robust statistics downstream.

use std::sync::Once;
use std::time::Instant;

use fmn_anim::{DeclaredOp, DeclaredUpdater};
use fmn_mobject::{Mobject, RecordBuffer, RecordSchema, Stage};
use fmn_scene::{RuntimeConfig, Scene};
use pyo3::prelude::*;
use pyo3::types::PyDict;

/// The four PG-8 scene classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pg8Class {
    /// Built-in mobjects + engine-executable updaters; no Python callback.
    NativeBuiltins,
    /// One Python updater per mobject, per frame, through `set_field`.
    PerFrameCallback,
    /// One Python updater per mobject, per frame, through the live
    /// zero-copy point view.
    PointTransformCallback,
    /// Dynamic Python subclasses: override-driven construction plus
    /// per-frame bound-method updater dispatch.
    DynamicSubclass,
}

impl Pg8Class {
    /// Fixed class order for producers and reports.
    pub const ALL: [Pg8Class; 4] = [
        Pg8Class::NativeBuiltins,
        Pg8Class::PerFrameCallback,
        Pg8Class::PointTransformCallback,
        Pg8Class::DynamicSubclass,
    ];

    /// Stable scenario name (the policy catalog's `scenario` column).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Pg8Class::NativeBuiltins => "native-builtins",
            Pg8Class::PerFrameCallback => "per-frame-callback",
            Pg8Class::PointTransformCallback => "point-transform-callback",
            Pg8Class::DynamicSubclass => "dynamic-subclass",
        }
    }

    /// Whether the timed repetition includes scene construction.
    #[must_use]
    fn rebuilds_per_repetition(self) -> bool {
        matches!(self, Pg8Class::DynamicSubclass)
    }
}

/// One retained workload repetition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepObservation {
    /// Wall time of one bridge-side workload repetition.
    pub elapsed_ns: u128,
    /// Wall time of the pure-Rust twin (`native-builtins` only).
    pub reference_ns: Option<u128>,
    /// Host-quality failure reason; the observation is retained regardless.
    pub invalid_reason: Option<String>,
}

/// Raw measurement output: retained repetitions plus the final scene
/// state bytes (self-golden input).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessRun {
    /// `warmup + repetitions` is excluded; exactly `repetitions` entries.
    pub repetitions: Vec<RepObservation>,
    /// Final bridge-side scene state, deterministic for a fixed workload.
    pub result_state: Vec<u8>,
    /// Final pure-Rust twin state (`native-builtins` only); must equal
    /// `result_state` — the twin runs bit-identical declared updaters.
    pub reference_state: Option<Vec<u8>>,
}

const PYTHON_PRELUDE: &str = r"
import numpy as np
import manimlib
from manimlib import Mobject, Scene
from manimlib import _NativeUpdater as NativeUpdater

VELOCITY = (0.1, -0.2, 0.3)

def state_bytes(scene):
    out = bytearray()
    for mob in scene.mobjects:
        fields = mob.field_names()
        for i in range(mob.n_records()):
            for f in fields:
                out += np.asarray(mob.get_field(f, i), dtype=np.float32).tobytes()
    return bytes(out)
";

const NATIVE_BUILTINS_SOURCE: &str = r"
def build(count):
    scene = Scene()
    mobs = []
    for _ in range(count):
        mob = Mobject()
        mob.resize(4)
        mobs.append(mob)
    scene.add(*mobs)
    scene._keep = mobs
    handles = [NativeUpdater.shift(mob, 'point', [0.1, -0.2, 0.3]) for mob in mobs]
    return scene, handles
";

const PER_FRAME_CALLBACK_SOURCE: &str = r"
def _make_updater():
    def update(mob, dt):
        x, y, z = mob.get_field('point', 0)
        mob.set_field('point', 0, [x + 0.1 * dt, y - 0.2 * dt, z + 0.3 * dt])
    return update

def build(count):
    scene = Scene()
    mobs = []
    for _ in range(count):
        mob = Mobject()
        mob.resize(4)
        mob.add_updater(_make_updater(), call=False)
        mobs.append(mob)
    scene.add(*mobs)
    scene._keep = mobs
    return scene
";

const POINT_TRANSFORM_CALLBACK_SOURCE: &str = r"
def _make_updater():
    velocity = np.array([0.1, -0.2, 0.3], dtype=np.float64)
    def update(mob, dt):
        points = mob.data['point']
        points[:] = points.astype(np.float64) + velocity * dt
    return update

def build(count):
    scene = Scene()
    mobs = []
    for _ in range(count):
        mob = Mobject()
        mob.resize(4)
        mob.add_updater(_make_updater(), call=False)
        mobs.append(mob)
    scene.add(*mobs)
    scene._keep = mobs
    return scene
";

const DYNAMIC_SUBCLASS_SOURCE: &str = r"
class DynamicPulse(Mobject):
    def init_points(self):
        self.resize(4)

    def init_uniforms(self):
        super().init_uniforms()
        self.uniforms['glow'] = 0.5

    def tick(self, m, dt):
        x, y, z = self.get_field('point', 0)
        self.set_field('point', 0, [x + 0.1 * dt, y - 0.2 * dt, z + 0.3 * dt])

def build(count):
    scene = Scene()
    mobs = []
    for _ in range(count):
        mob = DynamicPulse()
        mob.add_updater(mob.tick, call=False)
        mobs.append(mob)
    scene.add(*mobs)
    scene._keep = mobs
    return scene
";

impl Pg8Class {
    fn source(self) -> &'static str {
        match self {
            Pg8Class::NativeBuiltins => NATIVE_BUILTINS_SOURCE,
            Pg8Class::PerFrameCallback => PER_FRAME_CALLBACK_SOURCE,
            Pg8Class::PointTransformCallback => POINT_TRANSFORM_CALLBACK_SOURCE,
            Pg8Class::DynamicSubclass => DYNAMIC_SUBCLASS_SOURCE,
        }
    }
}

static PYTHON_INIT: Once = Once::new();

/// The harness rebuilds the process-global `sys.modules["manimlib"]` entry
/// per measurement; concurrent measurements would race. Serialize them.
static MEASURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The pure-Rust twin of the `native-builtins` workload: same mobjects,
/// same declared shift updaters, no interpreter in the loop.
struct RustTwin {
    scene: Scene,
}

impl RustTwin {
    fn build(mobjects: usize) -> Result<Self, String> {
        let mut scene = Scene::new(RuntimeConfig::default(), 0)
            .map_err(|error| format!("rust twin scene: {error}"))?;
        for _ in 0..mobjects {
            let buffer = RecordBuffer::new(RecordSchema::mobject(), 4);
            let mob = scene.stage_mut().add(Mobject::from_buffer(buffer));
            scene
                .stage_mut()
                .add_to_scene(mob)
                .map_err(|error| format!("rust twin root: {error}"))?;
            let updater = DeclaredUpdater::new(DeclaredOp::Shift {
                field: "point".to_owned(),
                velocity: vec![0.1, -0.2, 0.3],
            })
            .map_err(|error| format!("rust twin updater: {error}"))?;
            let mut cell = updater;
            scene
                .stage_mut()
                .add_dt_updater(
                    mob,
                    move |stage: &mut Stage, mob: fmn_mobject::Mob, dt: f64| {
                        if let Some(entry) = stage.get_mut(mob) {
                            cell.apply(&mut entry.buffer, dt);
                        }
                    },
                    false,
                )
                .map_err(|error| format!("rust twin registration: {error}"))?;
        }
        Ok(Self { scene })
    }

    fn step(&mut self, dt: f64, frames: usize) {
        for _ in 0..frames {
            self.scene.stage_mut().update(dt);
        }
    }

    fn state_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let roots = self.scene.stage().roots().to_vec();
        for mob in roots {
            let Some(entry) = self.scene.stage().get(mob) else {
                continue;
            };
            let fields: Vec<String> = entry
                .buffer
                .schema()
                .fields()
                .iter()
                .map(|field| field.name.clone())
                .collect();
            for index in 0..entry.buffer.len() {
                for field in &fields {
                    if let Some(values) = entry.buffer.read(index, field) {
                        for value in values {
                            out.extend_from_slice(&value.to_le_bytes());
                        }
                    }
                }
            }
        }
        out
    }
}

fn observation(
    elapsed: std::time::Duration,
    reference: Option<std::time::Duration>,
) -> RepObservation {
    let elapsed_ns = elapsed.as_nanos();
    let invalid_reason = if elapsed_ns == 0 {
        Some("zero elapsed time: clock resolution below workload".to_owned())
    } else {
        None
    };
    RepObservation {
        elapsed_ns,
        reference_ns: reference.map(|value| value.as_nanos()),
        invalid_reason,
    }
}

/// Measure one PG-8 scene class against the real bridge.
///
/// Builds the class's canonical workload (`mobjects` built-in mobjects),
/// runs `warmup` untimed repetitions, then times `repetitions`
/// repetitions of `frames` `scene.update(dt)` steps. `dynamic-subclass`
/// rebuilds its scene inside every timed repetition (construction is part
/// of the class's declared workload); the other classes build once.
///
/// # Errors
/// Returns a string describing any interpreter, bridge, or workload
/// construction failure before or during measurement.
pub fn measure(
    class: Pg8Class,
    repetitions: usize,
    warmup: usize,
    frames: usize,
    mobjects: usize,
    dt: f64,
) -> Result<HarnessRun, String> {
    let _guard = MEASURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    PYTHON_INIT.call_once(Python::initialize);
    Python::attach(|py| {
        let module = PyModule::new(py, "manimlib").map_err(|error| error.to_string())?;
        py.import("sys")
            .and_then(|sys| sys.getattr("modules"))
            .and_then(|modules| modules.set_item("manimlib", &module))
            .map_err(|error| error.to_string())?;
        crate::manimlib(py, &module).map_err(|error| error.to_string())?;

        let source = std::ffi::CString::new(format!("{}{}", PYTHON_PRELUDE, class.source()))
            .map_err(|_| "workload source contains NUL".to_owned())?;
        let globals = PyDict::new(py);
        globals
            .set_item(
                "__name__",
                pyo3::types::PyString::new(py, "__fmn_pg8_harness__"),
            )
            .map_err(|error| error.to_string())?;
        py.run(source.as_c_str(), Some(&globals), Some(&globals))
            .map_err(|error| format!("{} workload: {error}", class.name()))?;
        let build = globals
            .get_item("build")
            .and_then(|item| {
                item.ok_or_else(|| {
                    PyErr::new::<pyo3::exceptions::PyRuntimeError, _>("workload defines no build")
                })
            })
            .map_err(|error| error.to_string())?;
        let state_bytes = globals
            .get_item("state_bytes")
            .and_then(|item| {
                item.ok_or_else(|| {
                    PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                        "workload defines no state_bytes",
                    )
                })
            })
            .map_err(|error| error.to_string())?;

        let mut twin = (class == Pg8Class::NativeBuiltins)
            .then(|| RustTwin::build(mobjects))
            .transpose()?;

        // Build once for the classes whose construction is not timed.
        let mut scene: Option<Bound<'_, PyAny>> = None;
        let mut handles: Option<Bound<'_, PyAny>> = None;
        if !class.rebuilds_per_repetition() {
            let built = build
                .call1((mobjects,))
                .map_err(|error| format!("{} build: {error}", class.name()))?;
            if class == Pg8Class::NativeBuiltins {
                let pair = built
                    .extract::<(Bound<'_, PyAny>, Bound<'_, PyAny>)>()
                    .map_err(|error| error.to_string())?;
                scene = Some(pair.0);
                handles = Some(pair.1);
            } else {
                scene = Some(built);
            }
        }

        let total = warmup + repetitions;
        let mut observations = Vec::with_capacity(repetitions);
        for repetition in 0..total {
            let bridge_start = Instant::now();
            if class.rebuilds_per_repetition() {
                scene = Some(
                    build
                        .call1((mobjects,))
                        .map_err(|error| format!("{} rebuild: {error}", class.name()))?,
                );
            }
            let active = scene
                .as_ref()
                .ok_or_else(|| "workload scene missing".to_owned())?;
            for _ in 0..frames {
                active
                    .call_method1("update", (dt,))
                    .map_err(|error| format!("{} update: {error}", class.name()))?;
            }
            let bridge_elapsed = bridge_start.elapsed();
            let reference_elapsed = twin.as_mut().map(|twin| {
                let reference_start = Instant::now();
                twin.step(dt, frames);
                reference_start.elapsed()
            });
            if repetition >= warmup {
                observations.push(observation(bridge_elapsed, reference_elapsed));
            }
        }

        let active = scene
            .as_ref()
            .ok_or_else(|| "workload scene missing".to_owned())?;
        let result_state: Vec<u8> = state_bytes
            .call1((active,))
            .and_then(|bytes| bytes.extract())
            .map_err(|error| format!("{} state: {error}", class.name()))?;
        let reference_state = twin.as_ref().map(RustTwin::state_bytes);
        drop(handles);
        Ok(HarnessRun {
            repetitions: observations,
            result_state,
            reference_state,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_names_match_the_policy_catalog() {
        let names: Vec<&str> = Pg8Class::ALL.iter().map(|class| class.name()).collect();
        assert_eq!(
            names,
            [
                "native-builtins",
                "per-frame-callback",
                "point-transform-callback",
                "dynamic-subclass"
            ]
        );
    }

    #[test]
    fn rust_twin_runs_the_declared_shift() {
        let mut twin = RustTwin::build(2).expect("twin");
        twin.step(0.5, 2);
        let state = twin.state_bytes();
        assert!(!state.is_empty());
        // After two 0.5 s frames at velocity 0.1, record 0 lane x is 1.0.
        let lane = f32::from_le_bytes([state[0], state[1], state[2], state[3]]);
        let expected = (0.1 * 0.5 + 0.1 * 0.5) as f32;
        assert_eq!(lane, expected);
    }

    #[test]
    fn zero_elapsed_is_an_invalid_observation() {
        let sample = observation(std::time::Duration::ZERO, None);
        assert!(sample.invalid_reason.is_some());
        let sample = observation(std::time::Duration::from_nanos(7), None);
        assert!(sample.invalid_reason.is_none());
    }
}
