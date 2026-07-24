//! Marionette's persistence layer (§8.7, fm-879): arena snapshots as
//! durable bytes in fmn-hash's canonical container (§6.7), one mechanism
//! backing four consumers — SceneState (§13.1), Studio undo, replay-
//! journal barriers (§13.4), and the Gauntlet's geometry-snapshot
//! self-goldens (§16.3).
//!
//! Format guarantees, exactly the §6.7 policy:
//! - **Versioned schema ids** ([`SNAPSHOT_SCHEMA`] `FMNA/1`,
//!   [`SCENE_STATE_SCHEMA`] `FMNA/2`; the self-golden suites hold `FMNS`):
//!   additive-minor / breaking-major from day one — snapshots persist in
//!   caches and repro bundles.
//! - **Deterministic bytes**: canonical field order (schema order for
//!   record columns, slot order for the arena, no map anywhere), float
//!   canonicalization at the write boundary (`-0 → +0`, one NaN — the
//!   [`fmn_hash::Writer`] rule), so snapshot-hash equality is meaningful
//!   for the replay journal and the self-goldens.
//! - **Corruption detection and size limits on read**: the container's
//!   trailing SHA-256 and [`fmn_hash::Limits`], enforced by
//!   [`fmn_hash::Reader::open`] before any payload is touched.
//!
//! The honesty clause, stated where it binds (§8.7, §13.4): **updater
//! callables never serialize.** A durable snapshot records each updater's
//! identity — `(UpdaterId, kind)` — and nothing else; decode returns that
//! manifest ([`UpdaterManifest`]) alongside a [`Snapshot`] whose entries
//! carry no callables. Re-binding callables (and invalidating a barrier
//! when a callback's version hash changed) is the replay journal's job
//! (fm-y7u), which consumes these identities. A consequence worth knowing:
//! re-encoding a decoded snapshot of an updater-bearing stage yields
//! different bytes (the callables are gone); byte-level re-open
//! determinism holds exactly for callable-free states.
//!
//! Handles serialize as `(slot index, generation)` — the stage id is a
//! process-local mint, re-bound at decode against the target stage
//! ([`Snapshot::from_bytes`] takes the stage whose id decoded handles
//! adopt), so a repro bundle restores into any fresh arena.

use fmn_core::rng::Pcg64Dxsm;
use fmn_hash::{Digest, Limits, Reader, Schema, SerialError, UnknownPolicy, Writer, sha256};

use fmn_core::types::Vec3;

use crate::record::{RecordBuffer, RecordSchema};
use crate::shape::{ShapeSlot, ShapeTag};
use crate::stage::{Mob, Snapshot, SnapshotEntry, Stage, UpdaterFn};
use crate::uniforms::{JointType, Uniforms};

/// The arena-snapshot document: magic `FMNA`, schema id 1, version 1.1.
///
/// Minor 1.1 appended the per-entry semantic shape tag (§10.8); a 1.0
/// stream decodes with no tag, which is exactly what `General` means.
pub const SNAPSHOT_SCHEMA: Schema = Schema::new(*b"FMNA", 1, 1, 1);

/// The scene-state envelope: magic `FMNA`, schema id 2, version 1.1 — it
/// embeds a snapshot, so it moves with [`SNAPSHOT_SCHEMA`].
pub const SCENE_STATE_SCHEMA: Schema = Schema::new(*b"FMNA", 2, 1, 1);

/// Errors from snapshot decode.
#[derive(Debug, Clone, PartialEq)]
pub enum PersistError {
    /// The container refused the bytes (magic/schema/version/checksum/
    /// size/EOF — every variant named by [`SerialError`]).
    Serial(SerialError),
    /// The payload parsed but violates the document's own invariants.
    Malformed(&'static str),
    /// A shape-tag discriminant this build does not know. A newer writer
    /// is the likely cause, and guessing would fabricate geometry.
    UnknownShapeTag(u8),
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serial(e) => write!(f, "snapshot container refused: {e}"),
            Self::Malformed(what) => write!(f, "snapshot payload malformed: {what}"),
            Self::UnknownShapeTag(code) => {
                write!(f, "snapshot carries unknown shape-tag discriminant {code}")
            }
        }
    }
}

impl std::error::Error for PersistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Serial(e) => Some(e),
            Self::Malformed(_) | Self::UnknownShapeTag(_) => None,
        }
    }
}

impl From<SerialError> for PersistError {
    fn from(e: SerialError) -> Self {
        Self::Serial(e)
    }
}

/// An updater's serializable kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdaterKindTag {
    /// `f(stage, mob)`.
    NonDt,
    /// `f(stage, mob, dt)`.
    Dt,
}

/// The per-slot updater identities a durable snapshot records — the
/// §13.4 vocabulary the replay journal validates against when re-binding
/// callables. Slots appear in arena order; slots without updaters are
/// omitted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdaterManifest {
    /// `(slot index, [(updater id, kind)])` in arena order.
    pub entries: Vec<(u32, Vec<(u64, UpdaterKindTag)>)>,
}

/// What [`Snapshot::from_bytes`] yields: the restorable snapshot (no
/// callables) plus the updater identities that were attached when the
/// bytes were written.
pub struct DecodedSnapshot {
    /// The arena state; feed it to [`Stage::restore`].
    pub snapshot: Snapshot,
    /// The updater identities (see the module docs' honesty clause).
    pub updaters: UpdaterManifest,
}

// ------------------------------------------------------------- encoding

fn put_mob(w: &mut Writer, mob: Mob) {
    let (index, generation) = mob.parts();
    w.put_u32(index).put_u32(generation);
}

fn put_mob_opt(w: &mut Writer, mob: Option<Mob>) {
    match mob {
        Some(m) => {
            w.put_bool(true);
            put_mob(w, m);
        }
        None => {
            w.put_bool(false);
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
fn put_buffer(w: &mut Writer, buffer: &RecordBuffer) {
    let schema = buffer.schema();
    let fields = schema.fields();
    w.put_u16(fields.len() as u16);
    for field in fields {
        w.put_str(&field.name).put_u16(field.width as u16);
    }
    let put_names = |w: &mut Writer, names: &[String]| {
        w.put_u16(names.len() as u16);
        for name in names {
            w.put_str(name);
        }
    };
    put_names(w, schema.aligned_keys());
    put_names(w, schema.pointlike_keys());
    w.put_u32(buffer.len() as u32);
    for field in fields {
        if let Some(column) = buffer.read_column(&field.name) {
            for value in column {
                w.put_f32(value);
            }
        }
    }
    let locked = buffer.locked_keys();
    w.put_u16(locked.len() as u16);
    for name in locked {
        w.put_str(name);
    }
}

fn put_uniforms(w: &mut Writer, u: &Uniforms) {
    w.put_f64(u.is_fixed_in_frame);
    for v in u.shading {
        w.put_f64(v);
    }
    for plane in &u.clip_planes {
        for &v in plane {
            w.put_f64(v);
        }
    }
    w.put_f64(u.anti_alias_width);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    w.put_u8(u.joint_type.to_code() as u8);
    w.put_bool(u.flat_stroke)
        .put_bool(u.scale_stroke_with_zoom)
        .put_bool(u.stroke_behind)
        .put_bool(u.depth_test)
        .put_bool(u.use_winding_fill);
}

#[allow(clippy::cast_possible_truncation)]
fn put_entry(w: &mut Writer, entry: &SnapshotEntry) {
    put_buffer(w, &entry.buffer);
    w.put_u32(entry.submobjects.len() as u32);
    for &m in &entry.submobjects {
        put_mob(w, m);
    }
    w.put_u32(entry.parents.len() as u32);
    for &m in &entry.parents {
        put_mob(w, m);
    }
    // Updaters: identity + kind only — the honesty clause.
    w.put_u32(entry.updaters.len() as u32);
    for slot in &entry.updaters {
        w.put_u64(slot.id.raw());
        w.put_u8(match slot.func {
            UpdaterFn::NonDt(_) => 0,
            UpdaterFn::Dt(_) => 1,
        });
    }
    w.put_bool(entry.updating_suspended)
        .put_bool(entry.is_animating);
    match entry.tracker {
        Some(t) => {
            w.put_bool(true);
            w.put_u8(match t.kind {
                crate::dynamics::TrackerKind::Plain => 0,
                crate::dynamics::TrackerKind::Exponential => 1,
                crate::dynamics::TrackerKind::Complex => 2,
            });
            w.put_f64(t.lanes[0]).put_f64(t.lanes[1]);
        }
        None => {
            w.put_bool(false);
        }
    }
    put_mob_opt(w, entry.target);
    put_mob_opt(w, entry.saved_state);
    w.put_u64(entry.pins as u64).put_bool(entry.pending_delete);
    put_uniforms(w, &entry.uniforms);
    put_shape(w, &entry.shape);
}

/// The semantic shape tag (§10.8), added in schema minor 1.1.
///
/// The tag carries durable class configuration — `Line`'s `path_arc`
/// outlives every transform — so dropping it on a round trip would change
/// meaning, not just performance. Encoded as a discriminant plus its
/// payload, followed by the point revision its geometry was true at
/// (`u64::MAX` standing for "none", which only `General` uses).
fn put_shape(w: &mut Writer, slot: &ShapeSlot) {
    let put_point = |w: &mut Writer, p: Vec3| {
        w.put_f64(p[0]).put_f64(p[1]).put_f64(p[2]);
    };
    match slot.tag {
        ShapeTag::General => {
            w.put_u8(0);
        }
        ShapeTag::Line {
            start,
            end,
            path_arc,
            buff,
        } => {
            w.put_u8(1);
            put_point(w, start);
            put_point(w, end);
            w.put_f64(path_arc).put_f64(buff);
        }
        ShapeTag::Polyline { vertices, closed } => {
            w.put_u8(2);
            w.put_u64(vertices as u64).put_bool(closed);
        }
        ShapeTag::Arc {
            center,
            radius,
            start_angle,
            angle,
        } => {
            w.put_u8(3);
            put_point(w, center);
            w.put_f64(radius).put_f64(start_angle).put_f64(angle);
        }
        ShapeTag::Circle { center, radius } => {
            w.put_u8(4);
            put_point(w, center);
            w.put_f64(radius);
        }
        ShapeTag::Dot { center, radius } => {
            w.put_u8(5);
            put_point(w, center);
            w.put_f64(radius);
        }
        ShapeTag::Rect {
            center,
            width,
            height,
        } => {
            w.put_u8(6);
            put_point(w, center);
            w.put_f64(width).put_f64(height);
        }
        ShapeTag::RoundedRect {
            center,
            width,
            height,
            corner_radius,
        } => {
            w.put_u8(7);
            put_point(w, center);
            w.put_f64(width).put_f64(height).put_f64(corner_radius);
        }
    }
    w.put_u64(slot.point_revision.unwrap_or(u64::MAX));
}

fn get_shape(r: &mut Reader<'_>) -> Result<ShapeSlot, PersistError> {
    let point = |r: &mut Reader<'_>| -> Result<Vec3, PersistError> {
        Ok([r.get_f64()?, r.get_f64()?, r.get_f64()?])
    };
    let tag = match r.get_u8()? {
        0 => ShapeTag::General,
        1 => ShapeTag::Line {
            start: point(r)?,
            end: point(r)?,
            path_arc: r.get_f64()?,
            buff: r.get_f64()?,
        },
        2 => ShapeTag::Polyline {
            vertices: r.get_u64()? as usize,
            closed: r.get_bool()?,
        },
        3 => ShapeTag::Arc {
            center: point(r)?,
            radius: r.get_f64()?,
            start_angle: r.get_f64()?,
            angle: r.get_f64()?,
        },
        4 => ShapeTag::Circle {
            center: point(r)?,
            radius: r.get_f64()?,
        },
        5 => ShapeTag::Dot {
            center: point(r)?,
            radius: r.get_f64()?,
        },
        6 => ShapeTag::Rect {
            center: point(r)?,
            width: r.get_f64()?,
            height: r.get_f64()?,
        },
        7 => ShapeTag::RoundedRect {
            center: point(r)?,
            width: r.get_f64()?,
            height: r.get_f64()?,
            corner_radius: r.get_f64()?,
        },
        other => return Err(PersistError::UnknownShapeTag(other)),
    };
    let revision = r.get_u64()?;
    Ok(ShapeSlot {
        tag,
        point_revision: (revision != u64::MAX).then_some(revision),
    })
}

impl Snapshot {
    /// Serialize into the versioned canonical container.
    ///
    /// # Errors
    /// [`SerialError::SizeLimit`] when the state exceeds
    /// [`Limits::DEFAULT`].
    #[allow(clippy::cast_possible_truncation)]
    pub fn to_bytes(&self) -> Result<Vec<u8>, SerialError> {
        let mut w = Writer::new(SNAPSHOT_SCHEMA);
        w.put_u32(self.slots.len() as u32);
        for (generation, entry) in &self.slots {
            w.put_u32(*generation);
            match entry {
                Some(e) => {
                    w.put_bool(true);
                    put_entry(&mut w, e);
                }
                None => {
                    w.put_bool(false);
                }
            }
        }
        w.put_u32(self.free.len() as u32);
        for &index in &self.free {
            w.put_u32(index);
        }
        w.put_u32(self.roots.len() as u32);
        for &root in &self.roots {
            put_mob(&mut w, root);
        }
        w.finish()
    }

    /// The snapshot's content address: SHA-256 of its canonical bytes —
    /// what the replay journal compares at barriers.
    ///
    /// # Errors
    /// As [`Snapshot::to_bytes`].
    pub fn content_hash(&self) -> Result<Digest, SerialError> {
        Ok(sha256(&self.to_bytes()?))
    }

    /// Decode a durable snapshot, re-binding every handle to `stage`'s
    /// mint. Feed the result to [`Stage::restore`] on that same stage.
    ///
    /// # Errors
    /// [`PersistError::Serial`] (container), [`PersistError::Malformed`]
    /// (payload invariants).
    pub fn from_bytes(bytes: &[u8], stage: &Stage) -> Result<DecodedSnapshot, PersistError> {
        let stage_id = stage.stage_id();
        let mut r = Reader::open(
            bytes,
            SNAPSHOT_SCHEMA,
            Limits::DEFAULT,
            UnknownPolicy::Strict,
        )?;
        let get_mob = |r: &mut Reader<'_>| -> Result<Mob, PersistError> {
            let index = r.get_u32()?;
            let generation = r.get_u32()?;
            Ok(Mob::from_parts(stage_id, index, generation))
        };
        let get_mob_opt = |r: &mut Reader<'_>| -> Result<Option<Mob>, PersistError> {
            if r.get_bool()? {
                let index = r.get_u32()?;
                let generation = r.get_u32()?;
                Ok(Some(Mob::from_parts(stage_id, index, generation)))
            } else {
                Ok(None)
            }
        };

        let slot_count = r.get_u32()? as usize;
        let mut slots = Vec::with_capacity(slot_count.min(65_536));
        let mut manifest = UpdaterManifest::default();
        for slot_index in 0..slot_count {
            let generation = r.get_u32()?;
            let entry = if r.get_bool()? {
                // --- buffer
                let n_fields = r.get_u16()? as usize;
                let mut fields: Vec<(String, usize)> = Vec::with_capacity(n_fields);
                for _ in 0..n_fields {
                    let name = r.get_str()?.to_owned();
                    let width = r.get_u16()? as usize;
                    if width == 0 {
                        return Err(PersistError::Malformed("zero-width record field"));
                    }
                    fields.push((name, width));
                }
                let get_names = |r: &mut Reader<'_>| -> Result<Vec<String>, PersistError> {
                    let n = r.get_u16()? as usize;
                    let mut names = Vec::with_capacity(n);
                    for _ in 0..n {
                        names.push(r.get_str()?.to_owned());
                    }
                    Ok(names)
                };
                let aligned = get_names(&mut r)?;
                let pointlike = get_names(&mut r)?;
                let len = r.get_u32()? as usize;
                let field_refs: Vec<(&str, usize)> =
                    fields.iter().map(|(n, w)| (n.as_str(), *w)).collect();
                let aligned_refs: Vec<&str> = aligned.iter().map(String::as_str).collect();
                let pointlike_refs: Vec<&str> = pointlike.iter().map(String::as_str).collect();
                let schema = RecordSchema::new(&field_refs, &aligned_refs, &pointlike_refs);
                let mut buffer = RecordBuffer::new(schema, len);
                for (name, width) in &fields {
                    let lanes = len * width;
                    let mut column = Vec::with_capacity(lanes);
                    for _ in 0..lanes {
                        column.push(r.get_f32()?);
                    }
                    buffer.write_range(name, 0, &column);
                }
                let locked = get_names(&mut r)?;
                if !locked.is_empty() {
                    buffer.lock_data(locked.iter().map(String::as_str));
                }
                // --- graph + state
                let n_sub = r.get_u32()? as usize;
                let mut submobjects = Vec::with_capacity(n_sub.min(65_536));
                for _ in 0..n_sub {
                    submobjects.push(get_mob(&mut r)?);
                }
                let n_par = r.get_u32()? as usize;
                let mut parents = Vec::with_capacity(n_par.min(65_536));
                for _ in 0..n_par {
                    parents.push(get_mob(&mut r)?);
                }
                let n_upd = r.get_u32()? as usize;
                let mut ids = Vec::with_capacity(n_upd.min(65_536));
                for _ in 0..n_upd {
                    let id = r.get_u64()?;
                    let kind = match r.get_u8()? {
                        0 => UpdaterKindTag::NonDt,
                        1 => UpdaterKindTag::Dt,
                        _ => return Err(PersistError::Malformed("unknown updater kind")),
                    };
                    ids.push((id, kind));
                }
                if !ids.is_empty() {
                    #[allow(clippy::cast_possible_truncation)]
                    manifest.entries.push((slot_index as u32, ids));
                }
                let updating_suspended = r.get_bool()?;
                let is_animating = r.get_bool()?;
                let tracker = if r.get_bool()? {
                    let kind = match r.get_u8()? {
                        0 => crate::dynamics::TrackerKind::Plain,
                        1 => crate::dynamics::TrackerKind::Exponential,
                        2 => crate::dynamics::TrackerKind::Complex,
                        _ => return Err(PersistError::Malformed("unknown tracker kind")),
                    };
                    let lanes = [r.get_f64()?, r.get_f64()?];
                    Some(crate::dynamics::Tracker { kind, lanes })
                } else {
                    None
                };
                let target = get_mob_opt(&mut r)?;
                let saved_state = get_mob_opt(&mut r)?;
                let pins = usize::try_from(r.get_u64()?)
                    .map_err(|_| PersistError::Malformed("pin count overflows"))?;
                let pending_delete = r.get_bool()?;
                // --- uniforms (field order is the schema)
                let is_fixed_in_frame = r.get_f64()?;
                let mut shading = [0.0; 3];
                for lane in &mut shading {
                    *lane = r.get_f64()?;
                }
                let mut clip_planes = [[0.0; 4]; 4];
                for plane in &mut clip_planes {
                    for slot in plane {
                        *slot = r.get_f64()?;
                    }
                }
                let anti_alias_width = r.get_f64()?;
                let joint_type = JointType::from_code(f64::from(r.get_u8()?));
                let uniforms = Uniforms {
                    is_fixed_in_frame,
                    shading,
                    clip_planes,
                    anti_alias_width,
                    joint_type,
                    flat_stroke: r.get_bool()?,
                    scale_stroke_with_zoom: r.get_bool()?,
                    stroke_behind: r.get_bool()?,
                    depth_test: r.get_bool()?,
                    use_winding_fill: r.get_bool()?,
                };
                // Schema minor 1.1 appended the shape tag; a 1.0 stream
                // simply has no shape, which is what General means.
                let shape = if r.version().1 >= 1 {
                    get_shape(&mut r)?
                } else {
                    ShapeSlot::default()
                };
                Some(SnapshotEntry {
                    buffer,
                    submobjects,
                    parents,
                    updaters: Vec::new(), // callables never serialize
                    updating_suspended,
                    is_animating,
                    tracker,
                    target,
                    saved_state,
                    pins,
                    pending_delete,
                    uniforms,
                    shape,
                })
            } else {
                None
            };
            slots.push((generation, entry));
        }
        let n_free = r.get_u32()? as usize;
        let mut free = Vec::with_capacity(n_free.min(65_536));
        for _ in 0..n_free {
            free.push(r.get_u32()?);
        }
        let n_roots = r.get_u32()? as usize;
        let mut roots = Vec::with_capacity(n_roots.min(65_536));
        for _ in 0..n_roots {
            roots.push(get_mob(&mut r)?);
        }
        r.finish()?;
        Ok(DecodedSnapshot {
            snapshot: Snapshot { slots, free, roots },
            updaters: manifest,
        })
    }
}

impl Stage {
    /// [`Stage::snapshot`] straight to canonical bytes.
    ///
    /// # Errors
    /// As [`Snapshot::to_bytes`].
    pub fn snapshot_bytes(&self) -> Result<Vec<u8>, SerialError> {
        self.snapshot().to_bytes()
    }
}

// ----------------------------------------------------------- SceneState

/// The §13.1 scene-scope state: scene time, play count, the one RNG's
/// state words (fmn-core's export surface), and the arena snapshot. The
/// scene runtime (fm-5xm) captures and re-applies it; this layer owns the
/// bytes.
pub struct SceneState {
    /// Scene time at capture.
    pub time: f64,
    /// Completed `play()` count at capture.
    pub play_count: u64,
    /// `Pcg64Dxsm::state()`: `((state_hi, state_lo), (inc_hi, inc_lo))`.
    pub rng_state: ([u64; 2], [u64; 2]),
    /// The arena.
    pub snapshot: Snapshot,
}

/// A decoded scene state: the fields plus the snapshot's updater manifest.
pub struct DecodedSceneState {
    /// Scene time at capture.
    pub time: f64,
    /// Completed `play()` count at capture.
    pub play_count: u64,
    /// The RNG state words; [`DecodedSceneState::rng`] rebuilds the
    /// generator.
    pub rng_state: ([u64; 2], [u64; 2]),
    /// The arena, handles re-bound to the decoding stage.
    pub snapshot: Snapshot,
    /// The updater identities recorded at capture.
    pub updaters: UpdaterManifest,
}

impl DecodedSceneState {
    /// Rebuild the generator exactly where it was.
    #[must_use]
    pub fn rng(&self) -> Pcg64Dxsm {
        let (state, inc) = self.rng_state;
        Pcg64Dxsm::restore(state, inc)
    }
}

impl SceneState {
    /// Capture the scene-scope state from a stage, a play counter, and
    /// the RNG.
    #[must_use]
    pub fn capture(stage: &Stage, play_count: u64, rng: &Pcg64Dxsm) -> Self {
        Self {
            time: stage.time(),
            play_count,
            rng_state: rng.state(),
            snapshot: stage.snapshot(),
        }
    }

    /// Serialize the envelope (time, play count, RNG words, then the
    /// nested snapshot document as a length-prefixed field).
    ///
    /// # Errors
    /// As [`Snapshot::to_bytes`].
    pub fn to_bytes(&self) -> Result<Vec<u8>, SerialError> {
        let snapshot_bytes = self.snapshot.to_bytes()?;
        let mut w = Writer::new(SCENE_STATE_SCHEMA);
        w.put_f64(self.time).put_u64(self.play_count);
        let (state, inc) = self.rng_state;
        w.put_u64(state[0])
            .put_u64(state[1])
            .put_u64(inc[0])
            .put_u64(inc[1]);
        w.put_bytes(&snapshot_bytes);
        w.finish()
    }

    /// Decode an envelope, re-binding snapshot handles to `stage`.
    ///
    /// # Errors
    /// As [`Snapshot::from_bytes`].
    pub fn from_bytes(bytes: &[u8], stage: &Stage) -> Result<DecodedSceneState, PersistError> {
        let mut r = Reader::open(
            bytes,
            SCENE_STATE_SCHEMA,
            Limits::DEFAULT,
            UnknownPolicy::Strict,
        )?;
        let time = r.get_f64()?;
        let play_count = r.get_u64()?;
        let state = [r.get_u64()?, r.get_u64()?];
        let inc = [r.get_u64()?, r.get_u64()?];
        let snapshot_bytes = r.get_bytes()?.to_vec();
        r.finish()?;
        let decoded = Snapshot::from_bytes(&snapshot_bytes, stage)?;
        Ok(DecodedSceneState {
            time,
            play_count,
            rng_state: (state, inc),
            snapshot: decoded.snapshot,
            updaters: decoded.updaters,
        })
    }
}
