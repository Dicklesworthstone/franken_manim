//! The FMTL/1 timeline-bundle WRITER (fm-oee, §10.7 — the scene-side
//! exporter half of `docs/FMNT1_TIMELINE_BUNDLE.md`, the pinned contract).
//!
//! [`export_timeline_bundle`] consumes an authored [`Timeline`], drives it
//! through the ordinary segment drivers ([`play_segment`] /
//! [`wait_segment`] — the same primitives [`Timeline::render`] composes,
//! never a second engine), and serializes the result as one canonical
//! FMTL/1 document: the engine identity string, the fps, the nested
//! FMNA/5 [`TimelinePlan`], and one entry per segment. Pure segments the
//! exporter can *prove* reconstructible export as kind 0 (begin/end stage
//! snapshots plus the path/rate catalog tags); everything else exports as
//! kind 1 (one verbatim snapshot per frame).
//!
//! **The export-time proof is the contract's load-bearing rule.** A pure
//! play segment is nominated for kind 0 only when its whole normalized-alpha
//! pipeline collapses to the player's single-alpha law (no `time_span`,
//! zero lag, one shared catalog rate function, group run time equal to
//! every member's own). Nomination is never enough: the writer then
//! reconstructs EVERY frame of the segment through
//! [`fmn_anim::interpolate_between`] — the player's exact law, applied to
//! the snapshots round-tripped through their canonical bytes, so the proof
//! sees exactly what a player will decode — and requires bit-identity with
//! the engine's own emitted frame over everything interpolation can write
//! (record columns, placements, numeric uniforms), compared after the
//! container's float canonicalization. The contract demands one
//! mid-segment frame; proving every frame is strictly stronger and costs
//! only export time. Any mismatch, and the segment falls back to kind 1.
//! Never guessed.
//!
//! **Engine identity.** The recorded `engine_version` is
//! [`EngineIdentity::certified`]'s closure string — the same identity the
//! certified input closure journals. The bundle's content is front-end
//! state, and the front end's arithmetic IS the certified scalar
//! definition on every build tier (the SIMD lerp kernels are lane-for-lane
//! identical to it, by test), so the certified identity is the one string
//! a wasm player on any build can meaningfully refuse on.
//!
//! Determinism: fixed field order, canonical floats, no timestamps, no
//! host paths, no map iteration — two exports of the same scene run
//! produce identical bytes (locked by test).

use fmn_anim::bundle::{bundle_sub_alpha, interpolate_between};
use fmn_anim::frame::{FramePacket, play_segment, wait_segment};
use fmn_anim::purity::SegmentReport;
use fmn_anim::timeline::{Step, Timeline};
use fmn_anim::{AnimError, PathFunc, RateFunc, RationalFrameClock, rate_from_tag, rate_tag};
use fmn_core::rng::RngRoot;
use fmn_core::types::{canonicalize_f32, canonicalize_f64};
use fmn_hash::SerialError;
use fmn_hash::serial::{Limits, Schema, Writer};
use fmn_mobject::persist::PersistError;
use fmn_mobject::{Snapshot, Stage};
use fmn_render::engine::EngineIdentity;

/// The canonical container schema for an FMTL/1 timeline bundle — the
/// §6.7 registration for the timeline-bundle format family, id 1.
pub const TIMELINE_BUNDLE_SCHEMA: Schema = Schema::new(*b"FMTL", 1, 0, 0);

/// Default whole-export work ceiling. One million frames is more than nine
/// hours at 30 fps, while remaining a finite bound the exporter can reject
/// from the compiled plan before it advances the scene.
pub const DEFAULT_MAX_BUNDLE_EXPORT_FRAMES: u64 = 1_000_000;

/// Resource limits for one timeline-bundle export.
///
/// `max_frames` bounds semantic frame work. `max_capture_bytes` bounds the
/// cumulative destination tables and canonical snapshot bytes materialized
/// while capturing and proving segments. Charging is cumulative even when a
/// pure segment later releases its proof frames, so it bounds both peak
/// retained memory and total snapshot-serialization work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BundleExportLimits {
    /// Maximum total frames admitted by the compiled plan.
    pub max_frames: u64,
    /// Maximum cumulative bytes charged to capture/proof accumulation.
    pub max_capture_bytes: usize,
}

impl BundleExportLimits {
    /// Production defaults: one million frames and the canonical
    /// container's 256 MiB total-size ceiling for capture/proof storage.
    pub const DEFAULT: Self = Self {
        max_frames: DEFAULT_MAX_BUNDLE_EXPORT_FRAMES,
        max_capture_bytes: Limits::DEFAULT.max_total,
    };
}

impl Default for BundleExportLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The engine identity string recorded as the bundle's `engine_version`:
/// [`EngineIdentity::certified`]'s canonical closure string (see the
/// module docs for why the certified identity, not the build's fast tier).
#[must_use]
pub fn bundle_engine_version() -> String {
    EngineIdentity::certified().closure_string()
}

/// A bundle export failure.
#[derive(Debug)]
pub enum BundleError {
    /// A segment driver (or the clock beneath it) refused the run.
    Anim(AnimError),
    /// A canonical-container write refused (size limits).
    Serial(SerialError),
    /// A just-written snapshot failed its own decode during the proof —
    /// an exporter bug, surfaced loudly rather than masked as stateful.
    Persist(PersistError),
    /// The driver's segment report disagrees with the compiled plan —
    /// the two share one clock math, so a drift here is an engine bug.
    PlanDrift(&'static str),
    /// The compiled plan exceeds the declared whole-export work budget.
    /// This refusal happens before frame one and before scene mutation.
    FrameLimitExceeded {
        /// Frames scheduled by the compiled plan.
        frames: u64,
        /// Maximum frames admitted by the active limits.
        max_frames: u64,
    },
    /// Capture/proof accumulation exceeds its cumulative byte budget.
    CaptureLimitExceeded {
        /// The table or snapshot allocation being charged.
        context: &'static str,
        /// Cumulative bytes the operation would require.
        needed: usize,
        /// Active cumulative byte ceiling.
        limit: usize,
    },
    /// A bounded, precharged destination reservation failed.
    AllocationFailed {
        /// Destination table being reserved.
        context: &'static str,
        /// Elements requested after count validation.
        requested: usize,
    },
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anim(e) => write!(f, "timeline run failed: {e}"),
            Self::Serial(e) => write!(f, "bundle serialization refused: {e}"),
            Self::Persist(e) => write!(f, "snapshot round-trip failed during proof: {e}"),
            Self::PlanDrift(what) => write!(f, "segment report drifts from the plan: {what}"),
            Self::FrameLimitExceeded { frames, max_frames } => write!(
                f,
                "timeline bundle needs {frames} frames, exceeding the {max_frames}-frame export budget"
            ),
            Self::CaptureLimitExceeded {
                context,
                needed,
                limit,
            } => write!(
                f,
                "timeline bundle {context} needs {needed} cumulative bytes, exceeding the {limit}-byte capture budget"
            ),
            Self::AllocationFailed { context, requested } => write!(
                f,
                "timeline bundle {context} could not reserve {requested} validated entries"
            ),
        }
    }
}

impl std::error::Error for BundleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Anim(e) => Some(e),
            Self::Serial(e) => Some(e),
            Self::Persist(e) => Some(e),
            Self::PlanDrift(_)
            | Self::FrameLimitExceeded { .. }
            | Self::CaptureLimitExceeded { .. }
            | Self::AllocationFailed { .. } => None,
        }
    }
}

impl From<AnimError> for BundleError {
    fn from(e: AnimError) -> Self {
        Self::Anim(e)
    }
}

impl From<SerialError> for BundleError {
    fn from(e: SerialError) -> Self {
        Self::Serial(e)
    }
}

/// Cumulative capture/proof allocation account. It deliberately never
/// refunds a charge: released pure-proof frames reduce peak memory, while the
/// monotone account also bounds total serialization work across segments.
struct CaptureBudget {
    limit: usize,
    charged: usize,
}

impl CaptureBudget {
    fn new(limit: usize) -> Self {
        Self { limit, charged: 0 }
    }

    fn charge(&mut self, additional: usize, context: &'static str) -> Result<(), BundleError> {
        let needed =
            self.charged
                .checked_add(additional)
                .ok_or(BundleError::CaptureLimitExceeded {
                    context,
                    needed: usize::MAX,
                    limit: self.limit,
                })?;
        if needed > self.limit {
            return Err(BundleError::CaptureLimitExceeded {
                context,
                needed,
                limit: self.limit,
            });
        }
        self.charged = needed;
        Ok(())
    }

    fn reserve_exact<T>(
        &mut self,
        destination: &mut Vec<T>,
        additional: usize,
        context: &'static str,
    ) -> Result<(), BundleError> {
        let bytes = additional.checked_mul(std::mem::size_of::<T>()).ok_or(
            BundleError::CaptureLimitExceeded {
                context,
                needed: usize::MAX,
                limit: self.limit,
            },
        )?;
        self.charge(bytes, context)?;
        destination
            .try_reserve_exact(additional)
            .map_err(|_| BundleError::AllocationFailed {
                context,
                requested: additional,
            })
    }
}

/// One serialized segment entry.
enum SegmentEntry {
    /// Kind 0: reconstructible from begin/end snapshots through the
    /// player's record-lerp law — proven frame by frame at export time.
    Pure {
        begin: Vec<u8>,
        end: Vec<u8>,
        path: u8,
        rate: u8,
    },
    /// Kind 1: every frame's snapshot, verbatim.
    Stateful { frames: Vec<Vec<u8>> },
}

/// Nominate a segment for kind-0 export: the pure classification the
/// drivers already computed, plus the pipeline shape whole-segment record
/// interpolation can represent — no `time_span`, zero lag, every member's
/// own run time equal to the group's (so the player's `k/fps · 1/run_time`
/// is the engine's per-animation divisor, bit for bit), and one shared
/// catalog rate function. The path is nominated [`PathFunc::Straight`]:
/// every `.animate` method animation is straight by construction
/// (`path_arc` is a named refusal there), and anything that is secretly
/// not straight fails the proof and falls back to kind 1.
fn nominate(step: &Step, report: &SegmentReport) -> Option<(PathFunc, RateFunc)> {
    if !report.purity.is_pure() {
        return None;
    }
    match step {
        Step::Wait(_) => Some((PathFunc::Straight, RateFunc::Base(fmn_core::rate::linear))),
        Step::Play(animations) => {
            let mut tag: Option<u8> = None;
            for animation in animations.iter() {
                let config = &animation.state().config;
                if config.lag_ratio != 0.0 || config.time_span.is_some() {
                    return None;
                }
                if config.run_time.to_bits() != report.run_time.to_bits() {
                    return None;
                }
                let this = rate_tag(&config.rate_func)?;
                match tag {
                    None => tag = Some(this),
                    Some(uniform) if uniform == this => {}
                    Some(_) => return None,
                }
            }
            Some((PathFunc::Straight, rate_from_tag(tag?)?))
        }
    }
}

fn canonical_f32_slices_equal(left: &[f32], right: &[f32]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(&left, &right)| {
            canonicalize_f32(left).to_bits() == canonicalize_f32(right).to_bits()
        })
}

fn canonical_f64_equal(left: f64, right: f64) -> bool {
    canonicalize_f64(left).to_bits() == canonicalize_f64(right).to_bits()
}

/// Compare everything interpolation can write in two rooted forests without
/// constructing whole-frame fingerprints. Handles and arena ordinals remain
/// deliberately absent (they are process-local); record data, placements,
/// numeric uniforms, z-order, roots, and family structure compare in their
/// deterministic traversal order after container float canonicalization.
fn rooted_states_equal(left: &Stage, right: &Stage) -> bool {
    if left.roots().len() != right.roots().len() {
        return false;
    }
    for (&left_root, &right_root) in left.roots().iter().zip(right.roots()) {
        let left_family = left.family(left_root);
        let right_family = right.family(right_root);
        if left_family.len() != right_family.len() {
            return false;
        }
        for (left_mob, right_mob) in left_family.into_iter().zip(right_family) {
            let (Some(left_entry), Some(right_entry)) = (left.get(left_mob), right.get(right_mob))
            else {
                return false;
            };
            let left_fields = left_entry.buffer.schema().fields();
            let right_fields = right_entry.buffer.schema().fields();
            if left_fields.len() != right_fields.len() {
                return false;
            }
            for (left_field, right_field) in left_fields.iter().zip(right_fields) {
                if left_field != right_field {
                    return false;
                }
                match (
                    left_entry.buffer.read_column(&left_field.name),
                    right_entry.buffer.read_column(&right_field.name),
                ) {
                    (Some(left_column), Some(right_column))
                        if canonical_f32_slices_equal(&left_column, &right_column) => {}
                    _ => return false,
                }
            }
            if !left_entry
                .placement()
                .coefficients()
                .into_iter()
                .zip(right_entry.placement().coefficients())
                .all(|(left, right)| canonical_f64_equal(left, right))
            {
                return false;
            }
            let left_uniforms = left_entry.uniforms();
            let right_uniforms = right_entry.uniforms();
            if !canonical_f64_equal(
                left_uniforms.is_fixed_in_frame,
                right_uniforms.is_fixed_in_frame,
            ) || !left_uniforms
                .shading
                .into_iter()
                .zip(right_uniforms.shading)
                .all(|(left, right)| canonical_f64_equal(left, right))
                || !left_uniforms
                    .clip_planes
                    .into_iter()
                    .flatten()
                    .zip(right_uniforms.clip_planes.into_iter().flatten())
                    .all(|(left, right)| canonical_f64_equal(left, right))
                || !canonical_f64_equal(
                    left_uniforms.anti_alias_width,
                    right_uniforms.anti_alias_width,
                )
                || left_uniforms.joint_type != right_uniforms.joint_type
                || left.z_index(left_mob) != right.z_index(right_mob)
                || left_entry.submobjects().len() != right_entry.submobjects().len()
            {
                return false;
            }
        }
    }
    true
}

/// The export-time proof (the contract's bold rule): reconstruct every
/// frame of the segment from the round-tripped begin/end snapshots through
/// the player's exact law and require bit-identity with the engine's own
/// emitted frame. `Ok(false)` means "export this segment as kind 1".
fn prove_pure_segment(
    begin_bytes: &[u8],
    end_bytes: &[u8],
    path: PathFunc,
    rate: &RateFunc,
    frame_bytes: &[Vec<u8>],
    run_time: f64,
    fps: u32,
) -> Result<bool, BundleError> {
    // Decode exactly as the player will: fresh binding arena, strict
    // canonical container. The proof therefore covers the serialization
    // round-trip as well as the interpolation law.
    let binding = Stage::new();
    let begin = Snapshot::from_bytes(begin_bytes, &binding)
        .map_err(BundleError::Persist)?
        .snapshot;
    let end = Snapshot::from_bytes(end_bytes, &binding)
        .map_err(BundleError::Persist)?
        .snapshot;
    for (index, bytes) in frame_bytes.iter().enumerate() {
        let frame = i64::try_from(index + 1).unwrap_or(i64::MAX);
        // The engine's divisor chain, exactly: segment-local rational time
        // converted once, divided by the (nomination-verified) run time.
        let alpha = (frame as f64 / f64::from(fps)) / run_time;
        let reconstructed = interpolate_between(&begin, &end, bundle_sub_alpha(alpha, rate), path);
        let engine = Snapshot::from_bytes(bytes, &binding)
            .map_err(BundleError::Persist)?
            .snapshot
            .materialize();
        if !rooted_states_equal(&engine, &reconstructed) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn capture_snapshot_bytes(
    snapshot: &Snapshot,
    budget: &mut CaptureBudget,
    context: &'static str,
) -> Result<Vec<u8>, BundleError> {
    let bytes = snapshot.to_bytes()?;
    budget.charge(bytes.len(), context)?;
    Ok(bytes)
}

fn clone_snapshot_bytes(
    bytes: &[u8],
    budget: &mut CaptureBudget,
    context: &'static str,
) -> Result<Vec<u8>, BundleError> {
    budget.charge(bytes.len(), context)?;
    let mut cloned = Vec::new();
    cloned
        .try_reserve_exact(bytes.len())
        .map_err(|_| BundleError::AllocationFailed {
            context,
            requested: bytes.len(),
        })?;
    cloned.extend_from_slice(bytes);
    Ok(cloned)
}

fn classify_captured_segment(
    begin: Vec<u8>,
    mut frames: Vec<Vec<u8>>,
    path: PathFunc,
    rate: &RateFunc,
    run_time: f64,
    fps: u32,
    budget: &mut CaptureBudget,
) -> Result<SegmentEntry, BundleError> {
    let path_tag =
        fmn_anim::path_tag(path).ok_or(BundleError::PlanDrift("nominated path is untaggable"))?;
    let rate_tag = rate_tag(rate).ok_or(BundleError::PlanDrift("nominated rate is untaggable"))?;

    if frames.is_empty() {
        let end = clone_snapshot_bytes(&begin, budget, "frameless pure end snapshot")?;
        return if prove_pure_segment(&begin, &end, path, rate, &frames, run_time, fps)? {
            Ok(SegmentEntry::Pure {
                begin,
                end,
                path: path_tag,
                rate: rate_tag,
            })
        } else {
            Ok(SegmentEntry::Stateful { frames })
        };
    }

    let proven = {
        let end = frames.last().ok_or(BundleError::PlanDrift(
            "captured segment lost its last frame",
        ))?;
        prove_pure_segment(&begin, end, path, rate, &frames, run_time, fps)?
    };
    if proven {
        let end = frames.pop().ok_or(BundleError::PlanDrift(
            "captured segment lost its last frame",
        ))?;
        Ok(SegmentEntry::Pure {
            begin,
            end,
            path: path_tag,
            rate: rate_tag,
        })
    } else {
        Ok(SegmentEntry::Stateful { frames })
    }
}

/// Export an authored timeline as one canonical FMTL/1 document.
///
/// The timeline is consumed: its schedule is compiled and budget-checked
/// first (pure — the plan that nests into the bundle), then its steps drive
/// the public segment drivers against `stage` in authored order. Each packet
/// is serialized immediately into budgeted canonical snapshot bytes; whole
/// `FramePacket` tables are never retained. Per segment, in plan order:
///
/// - **Nominated pure segments** (see [`nominate`]) run the export-time
///   proof ([`prove_pure_segment`]); passing segments export as kind 0
///   with the begin snapshot the driver recorded and the segment's last
///   emitted frame as the end snapshot.
/// - Everything else — stateful classifications, unrepresentable
///   pipelines, proof failures — exports as kind 1 with one verbatim
///   snapshot per emitted frame.
///
/// # Errors
/// [`BundleError`] — resource-limit refusals, driver failures, container size
/// limits, a snapshot round-trip failure during the proof, or a report/plan
/// drift (all named; this function never panics on scene input).
pub fn export_timeline_bundle(
    timeline: Timeline,
    stage: &mut Stage,
    rng: &RngRoot,
) -> Result<Vec<u8>, BundleError> {
    export_timeline_bundle_with_limits(timeline, stage, rng, BundleExportLimits::DEFAULT)
}

/// Export with explicit frame-work and capture-memory limits.
///
/// The compiled plan is checked against `limits.max_frames` before the first
/// frame mutates `stage`. Capture tables and canonical snapshots are charged
/// cumulatively against `limits.max_capture_bytes` before retention, and all
/// proportional table reservations are fallible.
///
/// # Errors
/// As [`export_timeline_bundle`], plus the explicit resource variants of
/// [`BundleError`].
pub fn export_timeline_bundle_with_limits(
    timeline: Timeline,
    stage: &mut Stage,
    rng: &RngRoot,
    limits: BundleExportLimits,
) -> Result<Vec<u8>, BundleError> {
    let plan = timeline.compile()?;
    let total_frames = u64::try_from(plan.total_frames())
        .map_err(|_| BundleError::PlanDrift("compiled plan has a negative frame total"))?;
    let host_frame_limit = u64::try_from(usize::MAX).unwrap_or(u64::MAX);
    let max_frames = limits.max_frames.min(host_frame_limit);
    if total_frames > max_frames {
        return Err(BundleError::FrameLimitExceeded {
            frames: total_frames,
            max_frames,
        });
    }
    let fps = timeline.fps();
    let (mut steps, _labels) = timeline.into_steps();
    if steps.len() != plan.segments().len() {
        return Err(BundleError::PlanDrift("compiled segment count"));
    }

    let mut budget = CaptureBudget::new(limits.max_capture_bytes);
    let mut clock = RationalFrameClock::new(fps).map_err(AnimError::Clock)?;
    let mut entries: Vec<SegmentEntry> = Vec::new();
    budget.reserve_exact(&mut entries, steps.len(), "segment entry table")?;
    for (index, step) in steps.iter_mut().enumerate() {
        let planned = plan.segments().get(index).ok_or(BundleError::PlanDrift(
            "the run produced more segments than the plan",
        ))?;
        let expected_frames = usize::try_from(planned.n_frames)
            .map_err(|_| BundleError::PlanDrift("segment frame count exceeds host width"))?;
        let mut frames: Vec<Vec<u8>> = Vec::new();
        budget.reserve_exact(&mut frames, expected_frames, "captured frame table")?;
        let mut capture_error = None;
        let report = {
            let mut capture = |packet: FramePacket| {
                if capture_error.is_some() {
                    return;
                }
                if frames.len() >= expected_frames {
                    capture_error = Some(BundleError::PlanDrift(
                        "the run emitted more frames than the plan",
                    ));
                    return;
                }
                match packet.state().to_bytes() {
                    Ok(bytes) => match budget.charge(bytes.len(), "captured frame snapshot") {
                        Ok(()) => frames.push(bytes),
                        Err(error) => capture_error = Some(error),
                    },
                    Err(error) => capture_error = Some(BundleError::Serial(error)),
                }
            };
            match step {
                Step::Play(animations) => {
                    play_segment(stage, &mut clock, rng, animations, false, &mut capture)?
                }
                Step::Wait(duration) => {
                    wait_segment(stage, &mut clock, rng, *duration, None, false, &mut capture)?
                }
            }
        };
        if let Some(error) = capture_error {
            return Err(error);
        }
        if report.base_frame != planned.base_frame {
            return Err(BundleError::PlanDrift("segment base frame"));
        }
        if report.n_frames != planned.n_frames {
            return Err(BundleError::PlanDrift("segment frame count"));
        }
        if frames.len() != expected_frames {
            return Err(BundleError::PlanDrift("captured frame count"));
        }

        let entry = match nominate(step, &report) {
            Some((path, rate)) => {
                let begin_bytes = report
                    .begin_state
                    .as_ref()
                    .map(|snapshot| {
                        capture_snapshot_bytes(snapshot, &mut budget, "pure begin snapshot")
                    })
                    .transpose()?;
                match begin_bytes {
                    Some(begin) => classify_captured_segment(
                        begin,
                        frames,
                        path,
                        &rate,
                        planned.run_time,
                        fps,
                        &mut budget,
                    )?,
                    // A pure segment with no recorded begin state (a
                    // frameless schedule entry) records nothing at all.
                    None => SegmentEntry::Stateful { frames },
                }
            }
            None => SegmentEntry::Stateful { frames },
        };
        entries.push(entry);
    }

    let mut writer = Writer::new(TIMELINE_BUNDLE_SCHEMA);
    writer.put_str(&bundle_engine_version());
    writer.put_u32(fps);
    writer.put_bytes(&plan.to_bytes()?);
    writer.put_u32(wire_count(entries.len())?);
    for entry in &entries {
        match entry {
            SegmentEntry::Pure {
                begin,
                end,
                path,
                rate,
            } => {
                writer.put_u8(0);
                writer.put_bytes(begin);
                writer.put_bytes(end);
                writer.put_u8(*path);
                writer.put_u8(*rate);
            }
            SegmentEntry::Stateful { frames } => {
                writer.put_u8(1);
                writer.put_u32(wire_count(frames.len())?);
                for frame in frames {
                    writer.put_bytes(frame);
                }
            }
        }
    }
    Ok(writer.finish()?)
}

fn wire_count(needed: usize) -> Result<u32, SerialError> {
    u32::try_from(needed).map_err(|_| SerialError::SizeLimit {
        limit: usize::try_from(u32::MAX).unwrap_or(usize::MAX),
        needed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_anim::timeline::TimelinePlan;
    use fmn_anim::{Transform, prepare_animation};
    use fmn_core::rate;
    use fmn_hash::serial::{Limits, Reader, UnknownPolicy};
    use fmn_mobject::animate::AnimateArgs;
    use fmn_mobject::record::{RecordBuffer, RecordSchema};
    use fmn_mobject::{Mob, Mobject};

    /// A minimal filled vmobject: points plus the style columns, over the
    /// library's record schema. No geometry crate needed at this tier.
    fn test_mobject(points: &[[f64; 3]]) -> Mobject {
        let n = points.len();
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), n).unwrap();
        let flat: Vec<f32> = points
            .iter()
            .flat_map(|p| p.iter().map(|v| *v as f32))
            .collect();
        buffer.write_range("point", 0, &flat);
        let white: Vec<f32> = (0..n).flat_map(|_| [1.0, 1.0, 1.0, 1.0]).collect();
        buffer.write_range("stroke_rgba", 0, &white);
        let fill: Vec<f32> = (0..n).flat_map(|_| [0.5, 0.5, 0.5, 1.0]).collect();
        buffer.write_range("fill_rgba", 0, &fill);
        let widths = vec![4.0f32; n];
        buffer.write_range("stroke_width", 0, &widths);
        Mobject::from_buffer(buffer)
    }

    const TRIANGLE: &[[f64; 3]] = &[[0.0, 1.0, 0.0], [-1.0, -1.0, 0.0], [1.0, -1.0, 0.0]];

    fn stage_with_mob() -> (Stage, Mob) {
        let mut stage = Stage::new();
        let mob = stage.add(test_mobject(TRIANGLE));
        stage.add_to_scene(mob).expect("fresh handle roots");
        (stage, mob)
    }

    /// Read the container's top-level fields with the format's own Reader
    /// (test-side inspection, not a second parser): engine version, fps,
    /// the nested plan, and the per-segment kind tags in authored order.
    fn inspect(bytes: &[u8]) -> (String, u32, TimelinePlan, Vec<u8>) {
        let mut reader = Reader::open(
            bytes,
            TIMELINE_BUNDLE_SCHEMA,
            Limits::DEFAULT,
            UnknownPolicy::Strict,
        )
        .expect("bundle container opens");
        let engine_version = reader.get_str().expect("engine version").to_owned();
        let fps = reader.get_u32().expect("fps");
        let plan =
            TimelinePlan::from_bytes(reader.get_bytes().expect("plan")).expect("nested plan");
        let segment_count = reader.get_u32().expect("segment count") as usize;
        let mut kinds = Vec::new();
        for _ in 0..segment_count {
            let kind = reader.get_u8().expect("kind");
            assert!(kind <= 1, "test bundle carries unknown kind {kind}");
            kinds.push(kind);
            if kind == 0 {
                reader.get_bytes().expect("begin");
                reader.get_bytes().expect("end");
                reader.get_u8().expect("path");
                reader.get_u8().expect("rate");
            } else {
                let frames = reader.get_u32().expect("frames");
                for _ in 0..frames {
                    reader.get_bytes().expect("frame snapshot");
                }
            }
        }
        reader.finish().expect("container finishes cleanly");
        (engine_version, fps, plan, kinds)
    }

    /// A dyadic Transform shift (exact in floating point) over a 1s play —
    /// the segment the export-time proof should pass as kind 0.
    fn transform_shift_timeline(stage: &mut Stage, mob: Mob) -> Timeline {
        let target = stage.copy_family(mob).expect("copy");
        stage.shift(target, [2.0, 0.0, 0.0]);
        let animation =
            prepare_animation(Transform::new(mob, target), stage).expect("transform prepares");
        let mut timeline = Timeline::new(30).expect("fps");
        timeline.play(vec![animation]).expect("play step");
        timeline
    }

    #[test]
    fn wire_count_accepts_u32_max_and_refuses_one_over() {
        let max = usize::try_from(u32::MAX).unwrap_or(usize::MAX);
        assert_eq!(wire_count(max).unwrap(), u32::try_from(max).unwrap());
        if let Some(one_over) = max.checked_add(1) {
            assert!(matches!(
                wire_count(one_over),
                Err(SerialError::SizeLimit { limit, needed })
                    if limit == max && needed == one_over
            ));
        }
    }

    #[test]
    fn export_frame_budget_refuses_before_frame_one() {
        let (mut stage, mob) = stage_with_mob();
        let updater_calls = std::rc::Rc::new(std::cell::Cell::new(0_u32));
        let observed_calls = std::rc::Rc::clone(&updater_calls);
        stage
            .add_updater(
                mob,
                move |_stage, _mob| observed_calls.set(observed_calls.get() + 1),
                false,
            )
            .expect("updater registers");
        let before = stage.snapshot().to_bytes().expect("snapshot");
        let mut timeline = Timeline::new(1).expect("fps");
        timeline.wait(2.0).expect("wait step");
        let rng = RngRoot::from_seed(0);
        let limits = BundleExportLimits {
            max_frames: 1,
            ..BundleExportLimits::DEFAULT
        };

        let error = export_timeline_bundle_with_limits(timeline, &mut stage, &rng, limits)
            .expect_err("two frames must exceed the one-frame export budget");
        assert!(matches!(
            error,
            BundleError::FrameLimitExceeded {
                frames: 2,
                max_frames: 1
            }
        ));
        assert_eq!(
            updater_calls.get(),
            0,
            "plan-level refusal must not enter the driver's initial updater pass"
        );
        assert_eq!(
            stage.snapshot().to_bytes().expect("snapshot after refusal"),
            before,
            "plan-level refusal must happen before frame-one mutation"
        );
    }

    #[test]
    fn captured_snapshot_accumulation_obeys_the_memory_budget() {
        let mut stage = Stage::new();
        let mut timeline = Timeline::new(1).expect("fps");
        timeline.wait(1.0).expect("wait step");
        let rng = RngRoot::from_seed(0);
        let table_bytes = std::mem::size_of::<SegmentEntry>()
            .checked_add(std::mem::size_of::<Vec<u8>>())
            .expect("two small table entries fit");
        let limits = BundleExportLimits {
            max_capture_bytes: table_bytes,
            ..BundleExportLimits::DEFAULT
        };

        let error = export_timeline_bundle_with_limits(timeline, &mut stage, &rng, limits)
            .expect_err("the first encoded snapshot must exceed the table-only budget");
        assert!(matches!(
            error,
            BundleError::CaptureLimitExceeded {
                context: "captured frame snapshot",
                needed,
                limit
            } if needed > limit && limit == table_bytes
        ));
    }

    #[test]
    fn a_provable_pure_play_exports_kind_zero() {
        let (mut stage, mob) = stage_with_mob();
        let timeline = transform_shift_timeline(&mut stage, mob);
        let rng = RngRoot::from_seed(0);
        let bytes = export_timeline_bundle(timeline, &mut stage, &rng).expect("export");
        let (engine, fps, plan, kinds) = inspect(&bytes);
        assert_eq!(engine, bundle_engine_version());
        assert_eq!(fps, 30);
        assert_eq!(plan.segments().len(), 1);
        assert_eq!(
            kinds,
            vec![0],
            "a straight Transform shift must prove reconstructible"
        );
    }

    #[test]
    fn a_pure_shared_root_transform_exports_kind_zero() {
        let mut stage = Stage::new();
        let shared = stage.add(test_mobject(TRIANGLE));
        let left = stage.add(Mobject::new());
        let right = stage.add(Mobject::new());
        stage.attach(left, shared).expect("left parent");
        stage.attach(right, shared).expect("right parent");
        stage
            .add_many_to_scene(&[left, right])
            .expect("shared roots");

        let target = stage.copy_family(shared).expect("target copy");
        stage.shift(target, [2.0, 0.0, 0.0]);
        let animation =
            prepare_animation(Transform::new(shared, target), &mut stage).expect("prepares");
        let mut timeline = Timeline::new(30).expect("fps");
        timeline.play(vec![animation]).expect("play step");

        let rng = RngRoot::from_seed(0);
        let bytes = export_timeline_bundle(timeline, &mut stage, &rng).expect("export");
        let (_, _, _, kinds) = inspect(&bytes);
        assert_eq!(
            kinds,
            vec![0],
            "shared scene roots must not force a representable transform into frame snapshots"
        );
    }

    #[test]
    fn two_exports_of_the_same_run_are_byte_identical() {
        let run = || {
            let (mut stage, mob) = stage_with_mob();
            let timeline = transform_shift_timeline(&mut stage, mob);
            let rng = RngRoot::from_seed(0);
            export_timeline_bundle(timeline, &mut stage, &rng).expect("export")
        };
        assert_eq!(run(), run(), "FMTL/1 bytes are deterministic");
    }

    #[test]
    fn a_pure_wait_exports_kind_zero() {
        let (mut stage, _mob) = stage_with_mob();
        let mut timeline = Timeline::new(30).expect("fps");
        timeline.wait(0.5).expect("wait step");
        let rng = RngRoot::from_seed(0);
        let bytes = export_timeline_bundle(timeline, &mut stage, &rng).expect("export");
        let (_, _, plan, kinds) = inspect(&bytes);
        assert_eq!(plan.segments().len(), 1);
        assert_eq!(kinds, vec![0], "a pure wait reconstructs trivially");
    }

    #[test]
    fn an_out_and_back_rate_fails_the_proof_and_exports_kind_one() {
        // there_and_back(1) == 0: the segment's last frame equals its
        // begin state, so begin→end record lerp can never reproduce the
        // mid-segment excursion. The proof must catch that — this is the
        // contract's "never guessed" fallback, exercised for real.
        let (mut stage, mob) = stage_with_mob();
        let args = AnimateArgs {
            rate_func: Some(rate::there_and_back),
            ..AnimateArgs::default()
        };
        let animation = mob
            .animate()
            .set_anim_args(args)
            .and_then(|b| b.shift([2.0, 0.0, 0.0]))
            .expect("animate chain builds");
        let animation = prepare_animation(animation, &mut stage).expect("prepares");
        let mut timeline = Timeline::new(30).expect("fps");
        timeline.play(vec![animation]).expect("play step");
        let rng = RngRoot::from_seed(0);
        let bytes = export_timeline_bundle(timeline, &mut stage, &rng).expect("export");
        let (_, _, _, kinds) = inspect(&bytes);
        assert_eq!(kinds, vec![1], "the proof must refuse this segment");
    }

    #[test]
    fn a_stateful_segment_exports_kind_one() {
        // A dt-updater demotes the segment to stateful by classification;
        // no proof is even attempted.
        let (mut stage, mob) = stage_with_mob();
        stage
            .add_dt_updater(
                mob,
                |stage, mob, dt| {
                    stage.shift(mob, [0.5 * dt, 0.0, 0.0]);
                },
                false,
            )
            .expect("updater registers");
        let timeline = transform_shift_timeline(&mut stage, mob);
        let rng = RngRoot::from_seed(0);
        let bytes = export_timeline_bundle(timeline, &mut stage, &rng).expect("export");
        let (_, _, _, kinds) = inspect(&bytes);
        assert_eq!(kinds, vec![1], "updaters are stateful by vocabulary");
    }

    #[test]
    fn labels_and_multiple_segments_keep_authored_order() {
        let (mut stage, mob) = stage_with_mob();
        let mut timeline = Timeline::new(30).expect("fps");
        timeline.label("start");
        let target = stage.copy_family(mob).expect("copy");
        stage.shift(target, [2.0, 0.0, 0.0]);
        let animation =
            prepare_animation(Transform::new(mob, target), &mut stage).expect("prepares");
        timeline.play(vec![animation]).expect("play step");
        timeline.label("held");
        timeline.wait(0.5).expect("wait step");
        let rng = RngRoot::from_seed(0);
        let bytes = export_timeline_bundle(timeline, &mut stage, &rng).expect("export");
        let (_, _, plan, kinds) = inspect(&bytes);
        assert_eq!(kinds.len(), 2);
        assert_eq!(plan.frame_of_label("start"), Some(1));
        assert_eq!(plan.frame_of_label("held"), Some(31));
        assert_eq!(plan.total_frames(), 45);
    }
}
