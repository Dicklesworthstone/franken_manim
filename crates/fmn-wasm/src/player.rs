//! The W5 tier-2 timeline-bundle player (fm-oee, §10.7): [`FmnPlayer`]
//! consumes an FMTL/1 bundle (`docs/FMNT1_TIMELINE_BUNDLE.md`, the pinned
//! contract — the writer half lives in `fmn_scene::timeline_bundle`) and
//! plays it with scrub/seek: decode → reconstruct the frame's stage →
//! render through the tier-1 Lumen path → canvas RGBA8. There is no scene
//! code in this process; the bundle is the whole input.
//!
//! **Reconstruction is the contract's normative law, shared — never
//! reimplemented here.** Pure (kind 0) segments rebuild a frame from the
//! begin/end stage snapshots at `a = rate(alpha)` through
//! [`fmn_anim::interpolate_between`], the same function the writer's
//! export-time proof verified bit-identity against; stateful (kind 1)
//! segments restore the frame's recorded snapshot verbatim. Alpha is the
//! engine's own divisor chain: the segment-local rational time converted
//! once (`k / fps`), divided by the plan segment's run time.
//!
//! **Refusals are named, never panics** (the contract's list): a malformed
//! container is the serial reader's typed error; an `engine_version`
//! mismatch is [`PlayerError::EngineMismatch`] and is checked FIRST,
//! before any other field is interpreted; any disagreement between the
//! container's tags and the nested FMNA/5 plan is
//! [`PlayerError::PlanInconsistent`].
//!
//! Frame indices at this boundary are 0-based (`0 .. frame_count - 1`),
//! matching [`crate::FmnScene`]; the nested plan's 1-based global frame is
//! an internal detail (`locate` maps between them).

use fmn_anim::bundle::{bundle_sub_alpha, interpolate_between, path_from_tag, rate_from_tag};
use fmn_anim::timeline::TimelinePlan;
use fmn_anim::{PathFunc, RateFunc};
use fmn_hash::SerialError;
use fmn_hash::serial::{Limits, Reader, UnknownPolicy};
use fmn_mobject::persist::PersistError;
use fmn_mobject::{Snapshot, Stage};
use fmn_scene::timeline_bundle::{TIMELINE_BUNDLE_SCHEMA, bundle_engine_version};
use wasm_bindgen::JsError;
use wasm_bindgen::prelude::wasm_bindgen;

use crate::render_stage_rgba8;

/// Every refusal the player can produce, named per the contract.
#[derive(Debug)]
pub enum PlayerError {
    /// The container (or a nested document's container) refused the bytes:
    /// framing, schema, version, checksum, limits — the serial reader's
    /// typed error, verbatim.
    Malformed(SerialError),
    /// The bundle was written for a different engine identity than this
    /// build's certified closure string. Checked first, before any other
    /// field is interpreted.
    EngineMismatch {
        /// The identity the bundle was written for.
        wanted: String,
        /// This build's identity.
        found: String,
    },
    /// The container's tags disagree with the nested plan (fps, segment
    /// count, kind/path/rate tags, recorded frame counts).
    PlanInconsistent(&'static str),
    /// A segment snapshot payload failed its canonical decode.
    Snapshot(PersistError),
    /// A frame index outside `0 .. frame_count`.
    FrameOutOfRange {
        /// The requested index.
        index: u32,
        /// The player's frame count.
        total: u32,
    },
    /// The viewport was never set (or was set invalid).
    Viewport(&'static str),
    /// The render path refused.
    Render(String),
}

impl std::fmt::Display for PlayerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(e) => write!(f, "malformed FMTL/1 container: {e}"),
            Self::EngineMismatch { wanted, found } => write!(
                f,
                "bundle was written for engine {wanted:?}; this player is {found:?}"
            ),
            Self::PlanInconsistent(what) => {
                write!(f, "bundle disagrees with its nested plan: {what}")
            }
            Self::Snapshot(e) => write!(f, "segment snapshot refused: {e}"),
            Self::FrameOutOfRange { index, total } => {
                write!(f, "frame index {index} out of range 0..{total}")
            }
            Self::Viewport(what) => write!(f, "viewport: {what}"),
            Self::Render(e) => write!(f, "render: {e}"),
        }
    }
}

impl std::error::Error for PlayerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Malformed(e) => Some(e),
            Self::Snapshot(e) => Some(e),
            _ => None,
        }
    }
}

/// One decoded segment entry.
enum SegmentData {
    /// Kind 0: begin/end snapshots plus the reconstruction identities.
    /// Boxed: a Snapshot is a whole arena, and the kind-1 arm is a Vec.
    Pure {
        begin: Box<Snapshot>,
        end: Box<Snapshot>,
        path: PathFunc,
        rate: RateFunc,
    },
    /// Kind 1: every frame's snapshot, decoded.
    Stateful { frames: Vec<Snapshot> },
}

/// The host-testable core: everything [`FmnPlayer`] exposes, with no
/// bindgen types in the way (the same separation [`crate::SceneBuild`]
/// keeps for tier 1).
pub(crate) struct PlayerCore {
    plan: TimelinePlan,
    segments: Vec<SegmentData>,
    engine_version: String,
    width: u32,
    height: u32,
    cursor: u32,
}

impl PlayerCore {
    /// Decode a bundle. Every snapshot is decoded eagerly against one
    /// binding arena, so a corrupt payload is refused at load, never at
    /// scrub time. The viewport starts unset; [`PlayerCore::set_viewport`]
    /// is the deliberate second step.
    fn load(bytes: &[u8]) -> Result<Self, PlayerError> {
        let mut reader = Reader::open(
            bytes,
            TIMELINE_BUNDLE_SCHEMA,
            Limits::DEFAULT,
            UnknownPolicy::Strict,
        )
        .map_err(PlayerError::Malformed)?;
        // The version refusal comes FIRST: nothing else in the document is
        // interpreted for an engine this build is not.
        let wanted = reader.get_str().map_err(PlayerError::Malformed)?.to_owned();
        let found = bundle_engine_version();
        if wanted != found {
            return Err(PlayerError::EngineMismatch { wanted, found });
        }
        let fps = reader.get_u32().map_err(PlayerError::Malformed)?;
        let plan = TimelinePlan::from_bytes(reader.get_bytes().map_err(PlayerError::Malformed)?)
            .map_err(|e| match e {
                fmn_anim::timeline::TimelineError::Serial(serial) => PlayerError::Malformed(serial),
                fmn_anim::timeline::TimelineError::Malformed(what) => {
                    PlayerError::PlanInconsistent(what)
                }
            })?;
        if plan.fps() != fps {
            return Err(PlayerError::PlanInconsistent("fps"));
        }
        let segment_count = reader.get_u32().map_err(PlayerError::Malformed)? as usize;
        if segment_count != plan.segments().len() {
            return Err(PlayerError::PlanInconsistent("segment count"));
        }
        let binding = Stage::new();
        let mut segments = Vec::with_capacity(segment_count);
        for planned in plan.segments() {
            let kind = reader.get_u8().map_err(PlayerError::Malformed)?;
            let segment = match kind {
                0 => {
                    let begin = reader.get_bytes().map_err(PlayerError::Malformed)?;
                    let end = reader.get_bytes().map_err(PlayerError::Malformed)?;
                    let path_tag = reader.get_u8().map_err(PlayerError::Malformed)?;
                    let rate_tag = reader.get_u8().map_err(PlayerError::Malformed)?;
                    let path =
                        path_from_tag(path_tag).ok_or(PlayerError::PlanInconsistent("path tag"))?;
                    let rate =
                        rate_from_tag(rate_tag).ok_or(PlayerError::PlanInconsistent("rate tag"))?;
                    let decode_snapshot = |bytes: &[u8]| -> Result<Box<Snapshot>, PlayerError> {
                        Ok(Box::new(
                            Snapshot::from_bytes(bytes, &binding)
                                .map_err(PlayerError::Snapshot)?
                                .snapshot,
                        ))
                    };
                    SegmentData::Pure {
                        begin: decode_snapshot(begin)?,
                        end: decode_snapshot(end)?,
                        path,
                        rate,
                    }
                }
                1 => {
                    let frame_count = reader.get_u32().map_err(PlayerError::Malformed)?;
                    if i64::from(frame_count) != planned.n_frames {
                        return Err(PlayerError::PlanInconsistent(
                            "stateful segment frame count",
                        ));
                    }
                    let mut frames = Vec::with_capacity(frame_count as usize);
                    for _ in 0..frame_count {
                        let bytes = reader.get_bytes().map_err(PlayerError::Malformed)?;
                        frames.push(
                            Snapshot::from_bytes(bytes, &binding)
                                .map_err(PlayerError::Snapshot)?
                                .snapshot,
                        );
                    }
                    SegmentData::Stateful { frames }
                }
                _ => return Err(PlayerError::PlanInconsistent("segment kind tag")),
            };
            segments.push(segment);
        }
        reader.finish().map_err(PlayerError::Malformed)?;
        Ok(Self {
            plan,
            segments,
            engine_version: found,
            width: 0,
            height: 0,
            cursor: 0,
        })
    }

    fn set_viewport(&mut self, width: u32, height: u32) -> Result<(), PlayerError> {
        if width == 0
            || height == 0
            || width > crate::MAX_DIMENSION
            || height > crate::MAX_DIMENSION
        {
            return Err(PlayerError::Viewport("each dimension must be in 1..=4096"));
        }
        self.width = width;
        self.height = height;
        Ok(())
    }

    fn frame_count(&self) -> u32 {
        u32::try_from(self.plan.total_frames()).unwrap_or(u32::MAX)
    }

    /// Reconstruct the stage at 0-based frame `index` — the contract's
    /// reconstruction law, via the plan's `locate`.
    fn stage_at(&self, index: u32) -> Result<Stage, PlayerError> {
        let total = self.frame_count();
        let global = i64::from(index) + 1;
        let (segment_index, offset) = self
            .plan
            .locate(global)
            .ok_or(PlayerError::FrameOutOfRange { index, total })?;
        let planned = &self.plan.segments()[segment_index];
        let fps = self.plan.fps();
        let mut stage = match &self.segments[segment_index] {
            SegmentData::Pure {
                begin,
                end,
                path,
                rate,
            } => {
                // The engine's divisor chain, exactly: segment-local
                // rational time converted once, divided by the run time.
                let alpha = (offset as f64 / f64::from(fps)) / planned.run_time;
                interpolate_between(begin, end, bundle_sub_alpha(alpha, rate), *path)
            }
            SegmentData::Stateful { frames } => {
                let snapshot = frames
                    .get(usize::try_from(offset - 1).unwrap_or(usize::MAX))
                    .ok_or(PlayerError::PlanInconsistent(
                        "stateful segment shorter than the plan",
                    ))?;
                snapshot.materialize()
            }
        };
        // The clock mirror the engine's own packets carry at capture:
        // global frame time, one exact-to-f64 conversion.
        stage.set_time_from_clock(global as f64 / f64::from(fps));
        Ok(stage)
    }

    /// Seek: validate and position the cursor. Reconstruction is O(frame)
    /// and order-independent for both kinds, so a seek never replays
    /// anything — the cursor is presentation state, not a replay head.
    fn seek_frame(&mut self, index: u32) -> Result<(), PlayerError> {
        let total = self.frame_count();
        if index >= total {
            return Err(PlayerError::FrameOutOfRange { index, total });
        }
        self.cursor = index;
        Ok(())
    }

    fn render_index(&self, index: u32) -> Result<Vec<u8>, PlayerError> {
        if self.width == 0 || self.height == 0 {
            return Err(PlayerError::Viewport("set_viewport before rendering"));
        }
        let stage = self.stage_at(index)?;
        let revision = u64::from(index) + 1;
        render_stage_rgba8(&stage, self.width, self.height, revision)
            .map_err(|e| PlayerError::Render(e.to_string()))
    }
}

/// A loaded FMTL/1 timeline bundle, ready to scrub and render frames to
/// RGBA8 pixels through the tier-1 Lumen path.
///
/// ```text
/// const player = FmnPlayer.from_bundle(await (await fetch("bundle.fmtl")).arrayBuffer());
/// player.set_viewport(canvas.width, canvas.height);
/// const pixels = player.render_frame(0);            // Uint8Array, w*h*4
/// player.render_into(1, scratch);                   // zero-copy scrubbing
/// ```
#[wasm_bindgen]
pub struct FmnPlayer {
    core: PlayerCore,
}

#[wasm_bindgen]
impl FmnPlayer {
    /// Decode an FMTL/1 bundle. Refuses (as a JS `Error`) on a malformed
    /// container, an engine-version mismatch, or any plan inconsistency —
    /// the contract's named refusals.
    ///
    /// # Errors
    /// `JsError` for any [`PlayerError`].
    #[wasm_bindgen]
    pub fn from_bundle(bytes: &[u8]) -> Result<FmnPlayer, JsError> {
        Ok(FmnPlayer {
            core: PlayerCore::load(bytes)?,
        })
    }

    /// Set the render viewport in canvas pixels (required before the first
    /// render; the bundle deliberately carries no dimensions — the canvas
    /// owns them).
    ///
    /// # Errors
    /// `JsError` for a dimension outside `1..=4096`.
    pub fn set_viewport(&mut self, width: u32, height: u32) -> Result<(), JsError> {
        Ok(self.core.set_viewport(width, height)?)
    }

    /// Canvas pixel width, once set.
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.core.width
    }

    /// Canvas pixel height, once set.
    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.core.height
    }

    /// Total frames in the timeline.
    #[wasm_bindgen(getter)]
    pub fn frame_count(&self) -> u32 {
        self.core.frame_count()
    }

    /// The schedule's frame rate.
    #[wasm_bindgen(getter)]
    pub fn fps(&self) -> u32 {
        self.core.plan.fps()
    }

    /// The timeline's exact duration on the frame grid, in seconds.
    #[wasm_bindgen(getter)]
    pub fn duration_seconds(&self) -> f64 {
        self.core.plan.duration()
    }

    /// The engine identity this bundle was written for (and this build
    /// verified itself against at load).
    #[wasm_bindgen(getter)]
    pub fn engine_version(&self) -> String {
        self.core.engine_version.clone()
    }

    /// The current scrub position (set by [`FmnPlayer::seek_frame`]).
    #[wasm_bindgen(getter)]
    pub fn current_frame(&self) -> u32 {
        self.core.cursor
    }

    /// The authored label names, in authored order.
    pub fn labels(&self) -> Vec<String> {
        self.core
            .plan
            .labels()
            .iter()
            .map(|label| label.name.clone())
            .collect()
    }

    /// The 0-based frame a label resolves to, if the name is authored.
    pub fn frame_of_label(&self, name: &str) -> Option<u32> {
        let frame = self.core.plan.frame_of_label(name)?;
        u32::try_from(frame - 1).ok()
    }

    /// The segment kind at `index` (0 = pure-reconstructible, 1 =
    /// stateful/recorded) — diagnostic surface for scrub UIs.
    ///
    /// # Errors
    /// `JsError` for an out-of-range segment index.
    pub fn segment_kind(&self, index: u32) -> Result<u32, JsError> {
        let segment = self
            .core
            .segments
            .get(index as usize)
            .ok_or_else(|| JsError::new("segment index out of range"))?;
        Ok(match segment {
            SegmentData::Pure { .. } => 0,
            SegmentData::Stateful { .. } => 1,
        })
    }

    /// Seek the scrub cursor to 0-based frame `index`. Cheap in every
    /// direction: pure segments reconstruct in O(1) from their begin/end
    /// snapshots and stateful segments restore one recorded snapshot, so
    /// no replay ever happens.
    ///
    /// # Errors
    /// `JsError` for an out-of-range index.
    pub fn seek_frame(&mut self, index: u32) -> Result<(), JsError> {
        Ok(self.core.seek_frame(index)?)
    }

    /// Render 0-based frame `index` to a fresh `width * height * 4` sRGB
    /// RGBA8 buffer (top row first, D-23).
    ///
    /// # Errors
    /// `JsError` for an unset viewport, out-of-range index, or a render
    /// failure.
    pub fn render_frame(&self, index: u32) -> Result<Vec<u8>, JsError> {
        Ok(self.core.render_index(index)?)
    }

    /// The zero-copy variant: render `index` into a caller-owned buffer of
    /// exactly `width * height * 4` bytes, so scrubbing never allocates a
    /// fresh frame per step.
    ///
    /// # Errors
    /// `JsError` for a wrong-length destination, unset viewport,
    /// out-of-range index, or a render failure.
    pub fn render_into(&self, index: u32, dst: &mut [u8]) -> Result<(), JsError> {
        let pixels = self.core.render_index(index)?;
        if dst.len() != pixels.len() {
            return Err(JsError::new(&format!(
                "render_into destination is {} bytes; expected {} ({}x{}x4)",
                dst.len(),
                pixels.len(),
                self.core.width,
                self.core.height
            )));
        }
        dst.copy_from_slice(&pixels);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo_bundle::{demo_bundle, demo_scene};

    fn loaded() -> PlayerCore {
        let bytes = demo_bundle().expect("demo bundle exports");
        PlayerCore::load(&bytes).expect("demo bundle loads")
    }

    #[test]
    fn the_demo_bundle_round_trips() {
        // (a) write → read round-trip: the plan survives nested, every
        // frame the plan schedules is renderable, and the rendered frames
        // actually reach the pixels.
        let mut core = loaded();
        assert!(core.frame_count() > 0);
        assert_eq!(core.plan.fps(), 30);
        core.set_viewport(96, 54).expect("viewport");
        let first = core.render_index(0).expect("first frame");
        assert_eq!(first.len(), 96 * 54 * 4);
        assert!(first.iter().any(|&b| b != 0), "frame 0 is not background");
        let last = core
            .render_index(core.frame_count() - 1)
            .expect("last frame");
        assert!(last.iter().any(|&b| b != 0));
        assert_ne!(first, last, "the timeline moves pixels");
    }

    #[test]
    fn two_loads_render_byte_identical_frames() {
        // (b) determinism: the writer's bytes are fixed (locked writer-side);
        // two independent decodes of them reconstruct identically.
        let bytes = demo_bundle().expect("export");
        let mut a = PlayerCore::load(&bytes).expect("load a");
        let mut b = PlayerCore::load(&bytes).expect("load b");
        a.set_viewport(96, 54).expect("viewport a");
        b.set_viewport(96, 54).expect("viewport b");
        for index in [0, a.frame_count() / 2, a.frame_count() - 1] {
            assert_eq!(
                a.render_index(index).expect("render a"),
                b.render_index(index).expect("render b"),
                "frame {index} differs across identical loads"
            );
        }
    }

    #[test]
    fn engine_version_mismatch_refuses_first() {
        // (c) a bundle naming another engine refuses with the named error —
        // and refuses before any other field is read (this synthetic bundle
        // truncates right after the version string; EngineMismatch must
        // still win over the EOF).
        let found = bundle_engine_version();
        let mut writer = fmn_hash::serial::Writer::new(TIMELINE_BUNDLE_SCHEMA);
        writer.put_str("certified-cpu:scalar:0");
        let bytes = writer.finish().expect("small enough");
        let result = PlayerCore::load(&bytes);
        assert!(
            matches!(result, Err(PlayerError::EngineMismatch { .. })),
            "a bundle naming another engine must refuse with EngineMismatch"
        );
        let Err(PlayerError::EngineMismatch { wanted, found: got }) = result else {
            return; // the assert above already failed the test with context
        };
        assert_eq!(wanted, "certified-cpu:scalar:0");
        assert_eq!(got, found);
    }

    #[test]
    fn malformed_containers_carry_the_typed_error() {
        assert!(matches!(
            PlayerCore::load(&[]),
            Err(PlayerError::Malformed(_))
        ));
        let bytes = demo_bundle().expect("export");
        let truncated = PlayerCore::load(&bytes[..bytes.len() - 8]);
        assert!(
            matches!(truncated, Err(PlayerError::Malformed(_))),
            "a truncated container is the serial reader's error"
        );
    }

    #[test]
    fn plan_inconsistencies_are_named() {
        // A well-formed container whose segment count disagrees with the
        // nested plan must refuse with PlanInconsistent, not a parse error.
        let bytes = demo_bundle().expect("export");
        let mut reader = Reader::open(
            &bytes,
            TIMELINE_BUNDLE_SCHEMA,
            Limits::DEFAULT,
            UnknownPolicy::Strict,
        )
        .expect("open");
        let engine = reader.get_str().expect("engine").to_owned();
        let fps = reader.get_u32().expect("fps");
        let plan = reader.get_bytes().expect("plan").to_vec();
        reader.get_u32().expect("segment count");
        let mut writer = fmn_hash::serial::Writer::new(TIMELINE_BUNDLE_SCHEMA);
        writer.put_str(&engine);
        writer.put_u32(fps);
        writer.put_bytes(&plan);
        writer.put_u32(0); // the plan has segments; the container claims none
        let forged = writer.finish().expect("small enough");
        assert!(matches!(
            PlayerCore::load(&forged),
            Err(PlayerError::PlanInconsistent("segment count"))
        ));
        // Same treatment for the fps field.
        let mut writer = fmn_hash::serial::Writer::new(TIMELINE_BUNDLE_SCHEMA);
        writer.put_str(&engine);
        writer.put_u32(fps + 1);
        writer.put_bytes(&plan);
        writer.put_u32(0);
        let forged = writer.finish().expect("small enough");
        assert!(matches!(
            PlayerCore::load(&forged),
            Err(PlayerError::PlanInconsistent("fps"))
        ));
    }

    #[test]
    fn the_demo_bundle_records_a_pure_play_and_a_pure_wait() {
        // (d, player side) the demo play (two straight Transforms, one
        // shared catalog rate) proves kind 0; the pure wait follows.
        let bytes = demo_bundle().expect("export");
        let core = PlayerCore::load(&bytes).expect("load");
        assert_eq!(core.segments.len(), 2);
        assert!(
            matches!(core.segments[0], SegmentData::Pure { .. }),
            "the play segment proved reconstructible"
        );
        assert!(
            matches!(core.segments[1], SegmentData::Pure { .. }),
            "the wait segment proved reconstructible"
        );
    }

    #[test]
    fn scrubbed_frames_match_the_native_engine_bit_for_bit() {
        // (e) SCRUB CORRECTNESS: the corpus scene through the player vs the
        // native engine. The engine truth is the same schedule run through
        // the public drivers (`Timeline::render`), each emitted packet
        // rendered through the one Lumen path both sides share.
        let (mut stage, mut timeline) = demo_scene().expect("demo scene");
        let rng = fmn_core::rng::RngRoot::from_seed(0);
        let mut packets = Vec::new();
        timeline
            .render(&mut stage, &rng, &mut |packet| packets.push(packet))
            .expect("engine run");

        let mut core = loaded();
        core.set_viewport(96, 54).expect("viewport");
        let total = core.frame_count();
        assert_eq!(packets.len() as u32, total, "frame counts agree");
        assert!(matches!(core.segments[0], SegmentData::Pure { .. }));

        // Sample densely on the pure play (every third frame covers many
        // mid-segment scrubs), both segment boundaries, the label frames,
        // and the wait's mid and end.
        let mut samples: Vec<u32> = (0..30).step_by(3).collect();
        for extra in [0, 1, 29, 30, 31, 37, 44, total - 1] {
            samples.push(extra);
        }
        samples.sort_unstable();
        samples.dedup();
        assert!(samples.len() >= 12, "a meaningful sample count");

        for index in samples {
            let engine_stage = packets[index as usize].materialize_stage();
            let engine = crate::render_stage_rgba8(&engine_stage, 96, 54, u64::from(index) + 1)
                .expect("engine render");
            let played = core.render_index(index).expect("player render");
            assert_eq!(
                fmn_hash::sha256(&played),
                fmn_hash::sha256(&engine),
                "frame {index} digests differ between player and native engine"
            );
        }
    }

    #[test]
    fn seek_labels_and_accessors_track_the_plan() {
        let mut core = loaded();
        assert_eq!(core.frame_count(), 45);
        assert!((core.plan.duration() - 1.5).abs() < 1e-12);
        assert_eq!(core.engine_version, bundle_engine_version());
        assert_eq!(
            core.plan
                .labels()
                .iter()
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>(),
            ["shift", "settle"]
        );
        assert_eq!(core.plan.frame_of_label("shift"), Some(1));
        assert_eq!(core.plan.frame_of_label("settle"), Some(31));
        assert!(core.seek_frame(44).is_ok());
        assert_eq!(core.cursor, 44);
        assert!(matches!(
            core.seek_frame(45),
            Err(PlayerError::FrameOutOfRange {
                index: 45,
                total: 45
            })
        ));
        assert!(core.stage_at(45).is_err());
        let unrendered = core.render_index(0);
        assert!(
            matches!(unrendered, Err(PlayerError::Viewport(_))),
            "rendering before set_viewport is a named refusal"
        );
    }

    /// The tier-2 browser smoke: like tier 1's, a manual procedure (no
    /// wasm test runner is sanctioned), kept honest by asserting the demo
    /// assets exist. Run it:
    ///
    /// ```text
    /// wasm-pack build --target web --out-dir ../../demo/wasm/pkg crates/fmn-wasm
    /// cargo run -p fmn-wasm --example export_bundle
    /// python3 -m http.server 8080 --directory demo/wasm
    /// # open http://localhost:8080/player.html — a canvas shows the
    /// # bundle scrubbed; the label buttons jump to segment boundaries
    /// ```
    #[test]
    #[ignore = "manual browser procedure; see the doc comment for the exact commands"]
    fn player_browser_smoke_manual() {
        let demo =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo/wasm/player.html");
        assert!(demo.exists(), "demo/wasm/player.html is missing");
        let bundle =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo/wasm/bundle.fmtl");
        assert!(
            bundle.exists(),
            "demo/wasm/bundle.fmtl is missing — cargo run -p fmn-wasm --example export_bundle"
        );
        // The committed bundle must be the current exporter's bytes.
        let on_disk = std::fs::read(&bundle).expect("bundle reads");
        assert_eq!(
            on_disk,
            demo_bundle().expect("export"),
            "bundle.fmtl drifted from the exporter — re-export it"
        );
    }

    /// (g) the bundle-size record: the demo bundle is deterministic, so
    /// its size is a hard number with the R19 headroom convention
    /// (recorded + 10%, `SIZE_BUDGET.tsv`).
    #[test]
    fn demo_bundle_size_within_budget() {
        let budget_text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("SIZE_BUDGET.tsv"),
        )
        .expect("SIZE_BUDGET.tsv reads");
        let budget = budget_text
            .lines()
            .filter(|line| !line.starts_with('#'))
            .find_map(|line| {
                let mut fields = line.split('\t');
                (fields.next() == Some("demo-timeline-bundle"))
                    .then(|| fields.nth(1).and_then(|b| b.parse::<u64>().ok()))
                    .flatten()
            })
            .expect("demo-timeline-bundle budget row exists");
        let bytes = demo_bundle().expect("export");
        assert!(
            bytes.len() as u64 <= budget,
            "demo bundle is {} bytes, over the {budget}-byte budget — \
             re-measure deliberately and update SIZE_BUDGET.tsv",
            bytes.len()
        );
    }

    #[test]
    fn unknown_tags_refuse_as_plan_inconsistent() {
        // Forge a bundle whose first segment carries kind 7.
        let bytes = demo_bundle().expect("export");
        let mut reader = Reader::open(
            &bytes,
            TIMELINE_BUNDLE_SCHEMA,
            Limits::DEFAULT,
            UnknownPolicy::Strict,
        )
        .expect("open");
        let engine = reader.get_str().expect("engine").to_owned();
        let fps = reader.get_u32().expect("fps");
        let plan = reader.get_bytes().expect("plan").to_vec();
        let segments = reader.get_u32().expect("count");
        let mut writer = fmn_hash::serial::Writer::new(TIMELINE_BUNDLE_SCHEMA);
        writer.put_str(&engine);
        writer.put_u32(fps);
        writer.put_bytes(&plan);
        writer.put_u32(segments);
        writer.put_u8(7);
        let forged = writer.finish().expect("small enough");
        assert!(matches!(
            PlayerCore::load(&forged),
            Err(PlayerError::PlanInconsistent("segment kind tag"))
        ));
    }
}
