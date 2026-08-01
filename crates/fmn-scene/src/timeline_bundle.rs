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
use fmn_hash::serial::{Schema, Writer};
use fmn_mobject::persist::PersistError;
use fmn_mobject::{Snapshot, Stage};
use fmn_render::engine::EngineIdentity;

/// The canonical container schema for an FMTL/1 timeline bundle — the
/// §6.7 registration for the timeline-bundle format family, id 1.
pub const TIMELINE_BUNDLE_SCHEMA: Schema = Schema::new(*b"FMTL", 1, 0, 0);

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
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anim(e) => write!(f, "timeline run failed: {e}"),
            Self::Serial(e) => write!(f, "bundle serialization refused: {e}"),
            Self::Persist(e) => write!(f, "snapshot round-trip failed during proof: {e}"),
            Self::PlanDrift(what) => write!(f, "segment report drifts from the plan: {what}"),
        }
    }
}

impl std::error::Error for BundleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Anim(e) => Some(e),
            Self::Serial(e) => Some(e),
            Self::Persist(e) => Some(e),
            Self::PlanDrift(_) => None,
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

/// The canonical fingerprint of everything interpolation can write in a
/// stage's rooted forest — record schema and columns, placements, numeric
/// uniforms, z-order, family structure — with floats canonicalized by the
/// container's own rule, so the comparison asks the honest question: "do
/// the bundle's canonical bytes reproduce this frame?" Handles and arena
/// ordinals are deliberately absent (they are process-local); the walk
/// order is the deterministic roots-then-family depth-first order.
fn rooted_fingerprint(stage: &Stage, out: &mut Vec<u8>) {
    for root in stage.roots() {
        for mob in stage.family(*root) {
            let Some(entry) = stage.get(mob) else {
                continue;
            };
            let schema = entry.buffer.schema();
            out.extend_from_slice(&(schema.fields().len() as u64).to_le_bytes());
            for field in schema.fields() {
                out.extend_from_slice(field.name.as_bytes());
                out.extend_from_slice(&(field.width as u64).to_le_bytes());
                if let Some(column) = entry.buffer.read_column(&field.name) {
                    for value in column {
                        out.extend_from_slice(&canonicalize_f32(value).to_bits().to_le_bytes());
                    }
                }
            }
            for coefficient in entry.placement().coefficients() {
                out.extend_from_slice(&canonicalize_f64(coefficient).to_bits().to_le_bytes());
            }
            let uniforms = entry.uniforms();
            out.extend_from_slice(
                &canonicalize_f64(uniforms.is_fixed_in_frame)
                    .to_bits()
                    .to_le_bytes(),
            );
            for lane in uniforms.shading {
                out.extend_from_slice(&canonicalize_f64(lane).to_bits().to_le_bytes());
            }
            for plane in uniforms.clip_planes {
                for coefficient in plane {
                    out.extend_from_slice(&canonicalize_f64(coefficient).to_bits().to_le_bytes());
                }
            }
            out.extend_from_slice(
                &canonicalize_f64(uniforms.anti_alias_width)
                    .to_bits()
                    .to_le_bytes(),
            );
            out.push(match uniforms.joint_type {
                fmn_mobject::JointType::NoJoint => 0,
                fmn_mobject::JointType::Auto => 1,
                fmn_mobject::JointType::Bevel => 2,
                fmn_mobject::JointType::Miter => 3,
            });
            out.extend_from_slice(&stage.z_index(mob).to_le_bytes());
            out.extend_from_slice(&(entry.submobjects().len() as u64).to_le_bytes());
        }
    }
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
    packets: &[FramePacket],
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
    for (index, packet) in packets.iter().enumerate() {
        let frame = i64::try_from(index + 1).unwrap_or(i64::MAX);
        // The engine's divisor chain, exactly: segment-local rational time
        // converted once, divided by the (nomination-verified) run time.
        let alpha = (frame as f64 / f64::from(fps)) / run_time;
        let reconstructed = interpolate_between(&begin, &end, bundle_sub_alpha(alpha, rate), path);
        let engine = packet.materialize_stage();
        let mut engine_bits = Vec::new();
        rooted_fingerprint(&engine, &mut engine_bits);
        let mut reconstructed_bits = Vec::new();
        rooted_fingerprint(&reconstructed, &mut reconstructed_bits);
        if engine_bits != reconstructed_bits {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Export an authored timeline as one canonical FMTL/1 document.
///
/// The timeline is consumed: its schedule is compiled first (pure — the
/// plan that nests into the bundle), then its steps drive the public
/// segment drivers against `stage` in authored order, capturing every
/// frame packet and segment report. Per segment, in plan order:
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
/// [`BundleError`] — driver failures, container size limits, a snapshot
/// round-trip failure during the proof, or a report/plan drift (all
/// named; this function never panics on scene input).
pub fn export_timeline_bundle(
    timeline: Timeline,
    stage: &mut Stage,
    rng: &RngRoot,
) -> Result<Vec<u8>, BundleError> {
    let plan = timeline.compile()?;
    let fps = timeline.fps();
    let (mut steps, _labels) = timeline.into_steps();

    let mut clock = RationalFrameClock::new(fps).map_err(AnimError::Clock)?;
    let mut entries: Vec<SegmentEntry> = Vec::with_capacity(steps.len());
    for (index, step) in steps.iter_mut().enumerate() {
        let mut packets: Vec<FramePacket> = Vec::new();
        let mut capture = |packet: FramePacket| packets.push(packet);
        let report = match step {
            Step::Play(animations) => {
                play_segment(stage, &mut clock, rng, animations, false, &mut capture)?
            }
            Step::Wait(duration) => {
                wait_segment(stage, &mut clock, rng, *duration, None, false, &mut capture)?
            }
        };
        let planned = plan.segments().get(index).ok_or(BundleError::PlanDrift(
            "the run produced more segments than the plan",
        ))?;
        if report.base_frame != planned.base_frame {
            return Err(BundleError::PlanDrift("segment base frame"));
        }
        if report.n_frames != planned.n_frames {
            return Err(BundleError::PlanDrift("segment frame count"));
        }

        let entry = match nominate(step, &report) {
            Some((path, rate)) => {
                let begin_bytes = report
                    .begin_state
                    .as_ref()
                    .map(|snapshot| snapshot.to_bytes())
                    .transpose()?;
                // The end state is the segment's last emitted frame —
                // pre-finish, so teardown bookkeeping (unlock, remover,
                // replacement) never enters the bundle. A frameless
                // segment's end is its begin.
                let end_bytes = match packets.last() {
                    Some(packet) => Some(packet.state().to_bytes()?),
                    None => begin_bytes.clone(),
                };
                match (begin_bytes, end_bytes) {
                    (Some(begin), Some(end)) => {
                        let path_tag = fmn_anim::path_tag(path)
                            .ok_or(BundleError::PlanDrift("nominated path is untaggable"))?;
                        let rate_tag = rate_tag(&rate)
                            .ok_or(BundleError::PlanDrift("nominated rate is untaggable"))?;
                        if prove_pure_segment(
                            &begin,
                            &end,
                            path,
                            &rate,
                            &packets,
                            planned.run_time,
                            fps,
                        )? {
                            SegmentEntry::Pure {
                                begin,
                                end,
                                path: path_tag,
                                rate: rate_tag,
                            }
                        } else {
                            stateful_entry(&packets)?
                        }
                    }
                    // A pure segment with no recorded begin state (a
                    // frameless schedule entry) records nothing at all.
                    _ => stateful_entry(&packets)?,
                }
            }
            None => stateful_entry(&packets)?,
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

/// The kind-1 record of a captured segment: every frame's canonical
/// snapshot bytes, in emission order.
fn stateful_entry(packets: &[FramePacket]) -> Result<SegmentEntry, BundleError> {
    let mut frames = Vec::with_capacity(packets.len());
    for packet in packets {
        frames.push(packet.state().to_bytes()?);
    }
    Ok(SegmentEntry::Stateful { frames })
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
