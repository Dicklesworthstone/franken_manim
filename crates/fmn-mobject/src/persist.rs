//! Marionette's persistence layer (§8.7, fm-879): arena snapshots as
//! durable bytes in fmn-hash's canonical container (§6.7), one mechanism
//! backing four consumers — SceneState (§13.1), Studio undo, replay-
//! journal barriers (§13.4), and the Gauntlet's geometry-snapshot
//! self-goldens (§16.3).
//!
//! Format guarantees, exactly the §6.7 policy:
//! - **Versioned schema ids** ([`SNAPSHOT_SCHEMA`] `FMNA/1` v1.11,
//!   [`SCENE_STATE_SCHEMA`] `FMNA/2` v2.0; the self-golden suites hold `FMNS`):
//!   additive-minor / breaking-major from day one — snapshots persist in
//!   caches and repro bundles.
//! - **Deterministic bytes**: canonical field order (schema order for
//!   record columns, slot order for the arena, no map anywhere), float
//!   canonicalization at the write boundary (`-0 → +0`, one NaN — the
//!   [`fmn_hash::Writer`] rule), so snapshot-hash equality is meaningful
//!   for the replay journal and the self-goldens.
//! - **Corruption detection and size limits on read**: the container's
//!   trailing SHA-256 and [`fmn_hash::Limits`], enforced by
//!   [`fmn_hash::Reader::open`] before any payload is touched, then the
//!   decoded-allocation budget ([`SnapshotLimits`], fm-vek.7): every
//!   fixed-width count is preflighted against both the remaining encoded
//!   bytes and the aggregate decoded-bytes ceiling before the destination
//!   storage it names is reserved.
//!
//! The honesty clause, stated where it binds (§8.7, §13.4): **updater
//! callables and native executable attributes never serialize.** A durable snapshot records each updater's
//! identity — `(UpdaterId, kind)` — and nothing else; decode returns that
//! manifest ([`UpdaterManifest`]) alongside a [`Snapshot`] whose entries
//! carry no callables. Re-binding callables (and invalidating a barrier
//! when a callback's version hash changed) is the replay journal's job
//! (fm-y7u), which consumes these identities. A consequence worth knowing:
//! re-encoding a decoded snapshot of an updater-bearing stage yields
//! different bytes while the callables are absent; rebinding every manifest
//! identity through [`Stage::restore_updater_bindings`] restores the original
//! canonical bytes. Byte-level re-open determinism holds directly for
//! callable-free states.
//! Native attributes registered with `add_dt_updater_with_state` must likewise
//! be reconstructed by replay or explicitly restored with `set_updater_state`;
//! the manifest alone does not describe an executable history.
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
use crate::stage::{
    FamilyCheckpoint, FamilyCheckpointEntry, Mob, Snapshot, SnapshotEntry, SnapshotEntryData,
    Stage, UpdaterFn, UpdaterId,
};
use crate::uniforms::{JointType, Uniforms};
use crate::{ImageColorSpace, ImageResource, ImageSampler, ImageWrap, Placement, RenderPrimitive};

/// The arena-snapshot document: magic `FMNA`, schema id 1, version 1.11.
///
/// Minor history: 1.1 appended the shape tag; 1.2 appended `z_index`; 1.3
/// preserves the monotonic updater-id cursor, including identities removed
/// before the snapshot; 1.4 appended the object→world placement table
/// (fm-7if); 1.5 appended durable renderer primitive/topology identity;
/// 1.6 appended immutable image descriptors and RGBA8 bytes; **1.7 appends
/// the per-buffer `_data_defaults` phantom record** (fm-laky): one
/// presence bool and, when set, exactly one stride of f32 lanes. **1.8 appends
/// durable dark-side image resources**, retaining the full 1.7 payload prefix.
/// **1.9 appends object-wide dot glow uniforms**. A writer uses 1.8 when
/// every glow is zero and no dot cloud is present; the corresponding payload
/// has exactly the 1.8 meaning and layout. Every dot cloud requires 1.9 so
/// older renderers cannot mistake the new packed dtype for the legacy one.
/// Older dot clouds' constant glow lanes promote exactly to the uniform.
/// Heterogeneous legacy lanes refuse: choosing one point's value would lose
/// the saved material, which cannot be represented by an object-wide uniform.
/// **1.10 adds camera-sorted UV-grid primitive identity**; its payload retains
/// the full 1.9 layout. Writers keep 1.8/1.9 for scenes without this primitive,
/// preserving the existing canonical bytes. Older readers refuse the new
/// version rather than silently dropping a surface's ordering semantics.
/// **1.11 appends invisible padding copies' source identities**. These plain
/// links retain their original generation even when the source was deleted;
/// they do not own it or serialize its Python attributes. Writers retain the
/// previous 1.8/1.9/1.10 bytes when no entry carries padding provenance.
pub const SNAPSHOT_SCHEMA: Schema = Schema::new(*b"FMNA", 1, 1, 11);
const SNAPSHOT_SCHEMA_WITH_SORTED_SURFACE: Schema = Schema::new(*b"FMNA", 1, 1, 10);
const SNAPSHOT_SCHEMA_WITH_GLOW: Schema = Schema::new(*b"FMNA", 1, 1, 9);
const SNAPSHOT_SCHEMA_WITHOUT_GLOW: Schema = Schema::new(*b"FMNA", 1, 1, 8);

/// The scene-state envelope: magic `FMNA`, schema id 2, version 2.0.
///
/// Version 2 replaces the lossy floating scene time with the exact rational
/// clock identity `(frames_elapsed, fps)`. It still embeds a
/// [`SNAPSHOT_SCHEMA`] document as its final field.
pub const SCENE_STATE_SCHEMA: Schema = Schema::new(*b"FMNA", 2, 2, 0);

/// A content-only family checkpoint: magic `FMNA`, schema id 3, version 1.0.
///
/// Members occur once in depth-first family order, beginning with the root.
/// Each carries its original `(slot, generation)`, records, placement, child
/// edges, animation flags, tracker, uniforms, shape and render material. No
/// arena allocation state, scene roots, clock, updater functions or executable
/// attributes, pins, parent lists, or target/saved-state links are encoded.
/// Decoding requires every member to exist in the restored target Stage; it
/// never allocates mobjects or resurrects a deleted identity.
pub const FAMILY_CHECKPOINT_SCHEMA: Schema = Schema::new(*b"FMNA", 3, 1, 0);

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
    /// The decoded-allocation budget (fm-vek.7, [`SnapshotLimits`]) refused
    /// a count *before* any reservation: the document asked for more
    /// destination or transient storage than the aggregate limit allows.
    AllocationLimit {
        /// The aggregate decoded-bytes ceiling in force.
        limit: usize,
        /// Aggregate bytes the decode would have charged.
        needed: usize,
        /// Which destination/transient structure tripped the budget.
        what: &'static str,
    },
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serial(e) => write!(f, "snapshot container refused: {e}"),
            Self::Malformed(what) => write!(f, "snapshot payload malformed: {what}"),
            Self::UnknownShapeTag(code) => {
                write!(f, "snapshot carries unknown shape-tag discriminant {code}")
            }
            Self::AllocationLimit {
                limit,
                needed,
                what,
            } => {
                write!(
                    f,
                    "snapshot decode budget exceeded by {what}: \
                     {needed} bytes charged against a {limit}-byte limit"
                )
            }
        }
    }
}

impl std::error::Error for PersistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Serial(e) => Some(e),
            Self::Malformed(_) | Self::UnknownShapeTag(_) | Self::AllocationLimit { .. } => None,
        }
    }
}

impl From<SerialError> for PersistError {
    fn from(e: SerialError) -> Self {
        Self::Serial(e)
    }
}

/// Decode-time allocation budget for [`Snapshot::from_bytes`] and
/// [`FamilyCheckpoint::from_bytes`] (fm-vek.7).
///
/// The encoded container is already capped by [`Limits::DEFAULT`]
/// (256 MiB), but decoding *amplifies*: a five-byte empty-slot record
/// becomes a full slot-table entry, and every fixed-width count names
/// destination storage the decoder is asked to materialize. This budget
/// charges each destination and transient allocation against one explicit
/// aggregate ceiling *before* the reservation happens, so a hostile or
/// corrupt count is a typed refusal ([`PersistError::AllocationLimit`]),
/// never a multi-GiB speculative allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotLimits {
    /// Maximum aggregate bytes of decoded destination + transient storage
    /// one snapshot decode may charge.
    pub max_total_decoded_bytes: usize,
}

impl SnapshotLimits {
    /// The default budget: four times the encoded container cap. Record
    /// payloads (the dominant term in any real scene) decode at
    /// essentially 1:1 against their encoding, so 4x leaves every
    /// runtime-reachable arena — including large sparse ones — far under
    /// the ceiling, while a `u32::MAX` slot count or a five-byte-per-slot
    /// empty-slot bomb trips it orders of magnitude before the allocation
    /// it named.
    pub const DEFAULT: Self = Self {
        max_total_decoded_bytes: 4 * Limits::DEFAULT.max_total,
    };
}

impl Default for SnapshotLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The running account of decoded destination + transient bytes: every
/// reservation charges here *before* it happens.
struct DecodeBudget {
    limit: usize,
    charged: usize,
}

impl DecodeBudget {
    fn new(limit: usize) -> Self {
        Self { limit, charged: 0 }
    }

    fn charge(&mut self, bytes: usize, what: &'static str) -> Result<(), PersistError> {
        let charged = self.charged.saturating_add(bytes);
        if charged > self.limit {
            return Err(PersistError::AllocationLimit {
                limit: self.limit,
                needed: charged,
                what,
            });
        }
        self.charged = charged;
        Ok(())
    }

    /// Reserve exactly `additional` elements, the budget already charged;
    /// an allocator refusal surfaces as the same typed budget error rather
    /// than an abort.
    fn reserve<T>(
        &self,
        vec: &mut Vec<T>,
        additional: usize,
        what: &'static str,
    ) -> Result<(), PersistError> {
        vec.try_reserve_exact(additional)
            .map_err(|_| PersistError::AllocationLimit {
                limit: self.limit,
                needed: self.charged,
                what,
            })
    }
}

/// Preflight one fixed-width count against *both* feasibility channels
/// (fm-vek.7): the encoded bytes it would take to actually carry that
/// many items, and the decoded-storage budget for the destination vector.
/// Runs before any reservation or iteration over the count.
fn preflight_count(
    r: &Reader<'_>,
    budget: &mut DecodeBudget,
    count: usize,
    encoded_each: usize,
    decoded_each: usize,
    what: &'static str,
) -> Result<(), PersistError> {
    let need = count.saturating_mul(encoded_each);
    if need > r.remaining() {
        return Err(PersistError::Serial(SerialError::UnexpectedEof {
            need,
            remaining: r.remaining(),
        }));
    }
    budget.charge(count.saturating_mul(decoded_each), what)
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

/// One updater identity resolved from durable slot coordinates into a live
/// Stage handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdaterIdentity {
    /// Mobject whose updater list contained this identity.
    pub mob: Mob,
    /// Original removal token.
    pub id: UpdaterId,
    /// Callable shape the replay registry must provide.
    pub kind: UpdaterKindTag,
}

impl UpdaterManifest {
    /// Resolve durable slot identities against the Stage used for decode.
    ///
    /// Results retain arena order and per-mobject updater execution order,
    /// ready for a replay owner's callback registry. The returned
    /// [`UpdaterId`] values are the original removal identities, not newly
    /// allocated substitutes.
    ///
    /// # Errors
    /// [`PersistError::Malformed`] if a manifest names a slot that is not live
    /// in `stage`, is not in canonical arena order, or carries a reserved or
    /// duplicate identity.
    pub fn identities(&self, stage: &Stage) -> Result<Vec<UpdaterIdentity>, PersistError> {
        let mut identities = Vec::new();
        let mut previous_slot = None;
        for (slot, updaters) in &self.entries {
            if previous_slot.is_some_and(|previous| previous >= *slot) {
                return Err(PersistError::Malformed(
                    "updater manifest slots are not strictly increasing",
                ));
            }
            if updaters.is_empty() {
                return Err(PersistError::Malformed(
                    "updater manifest contains an empty slot",
                ));
            }
            let mob = stage.mob_at_slot(*slot).ok_or(PersistError::Malformed(
                "updater manifest target is not live",
            ))?;
            let mut seen = Vec::with_capacity(updaters.len());
            for &(raw, kind) in updaters {
                if raw == 0 {
                    return Err(PersistError::Malformed(
                        "updater manifest carries the reserved zero identity",
                    ));
                }
                if seen.contains(&raw) {
                    return Err(PersistError::Malformed(
                        "updater manifest repeats an identity on one mobject",
                    ));
                }
                seen.push(raw);
                identities.push(UpdaterIdentity {
                    mob,
                    id: UpdaterId::from_raw(raw),
                    kind,
                });
            }
            previous_slot = Some(*slot);
        }
        Ok(identities)
    }
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

fn count_u16(value: usize) -> Result<u16, SerialError> {
    u16::try_from(value).map_err(|_| SerialError::SizeLimit {
        limit: usize::from(u16::MAX),
        needed: value,
    })
}

fn count_u32(value: usize) -> Result<u32, SerialError> {
    u32::try_from(value).map_err(|_| SerialError::SizeLimit {
        limit: usize::try_from(u32::MAX).unwrap_or(usize::MAX),
        needed: value,
    })
}

fn count_u64(value: usize) -> Result<u64, SerialError> {
    u64::try_from(value).map_err(|_| SerialError::SizeLimit {
        limit: usize::try_from(u64::MAX).unwrap_or(usize::MAX),
        needed: value,
    })
}

fn put_render_primitive(w: &mut Writer, primitive: RenderPrimitive) -> Result<(), SerialError> {
    match primitive {
        RenderPrimitive::Vector => {
            w.put_u8(0);
        }
        RenderPrimitive::SurfaceGrid { resolution } => {
            w.put_u8(1)
                .put_u64(count_u64(resolution.0)?)
                .put_u64(count_u64(resolution.1)?);
        }
        RenderPrimitive::TriangleMesh => {
            w.put_u8(2);
        }
        RenderPrimitive::DotCloud => {
            w.put_u8(3);
        }
        RenderPrimitive::ImageQuad => {
            w.put_u8(4);
        }
        RenderPrimitive::CameraSortedSurfaceGrid { resolution } => {
            w.put_u8(5)
                .put_u64(count_u64(resolution.0)?)
                .put_u64(count_u64(resolution.1)?);
        }
    }
    Ok(())
}

fn get_render_primitive(
    r: &mut Reader<'_>,
    allow_sorted_surface: bool,
) -> Result<RenderPrimitive, PersistError> {
    match r.get_u8()? {
        0 => Ok(RenderPrimitive::Vector),
        1 => Ok(RenderPrimitive::SurfaceGrid {
            resolution: (
                usize::try_from(r.get_u64()?)
                    .map_err(|_| PersistError::Malformed("surface u resolution overflows"))?,
                usize::try_from(r.get_u64()?)
                    .map_err(|_| PersistError::Malformed("surface v resolution overflows"))?,
            ),
        }),
        2 => Ok(RenderPrimitive::TriangleMesh),
        3 => Ok(RenderPrimitive::DotCloud),
        4 => Ok(RenderPrimitive::ImageQuad),
        5 if allow_sorted_surface => Ok(RenderPrimitive::CameraSortedSurfaceGrid {
            resolution: (
                usize::try_from(r.get_u64()?)
                    .map_err(|_| PersistError::Malformed("surface u resolution overflows"))?,
                usize::try_from(r.get_u64()?)
                    .map_err(|_| PersistError::Malformed("surface v resolution overflows"))?,
            ),
        }),
        _ => Err(PersistError::Malformed("unknown render primitive")),
    }
}

fn put_image_resource(w: &mut Writer, image: &ImageResource) {
    w.put_u32(image.width()).put_u32(image.height());
    match image.color_space() {
        ImageColorSpace::Srgb => {
            w.put_u8(0);
        }
        ImageColorSpace::Linear => {
            w.put_u8(1);
        }
        ImageColorSpace::Gamma(gamma) => {
            w.put_u8(2).put_u32(gamma);
        }
    }
    let put_wrap = |w: &mut Writer, wrap: ImageWrap| {
        w.put_u8(match wrap {
            ImageWrap::Repeat => 0,
            ImageWrap::ClampToEdge => 1,
            ImageWrap::MirroredRepeat => 2,
        });
    };
    put_wrap(w, image.sampler().wrap_u);
    put_wrap(w, image.sampler().wrap_v);
    w.put_digest(&image.content_digest())
        .put_bytes(image.pixels());
}

fn get_image_resource(
    r: &mut Reader<'_>,
    budget: &mut DecodeBudget,
) -> Result<ImageResource, PersistError> {
    let width = r.get_u32()?;
    let height = r.get_u32()?;
    let color_space = match r.get_u8()? {
        0 => ImageColorSpace::Srgb,
        1 => ImageColorSpace::Linear,
        2 => ImageColorSpace::Gamma(r.get_u32()?),
        _ => return Err(PersistError::Malformed("unknown image color space")),
    };
    let get_wrap = |r: &mut Reader<'_>| -> Result<ImageWrap, PersistError> {
        match r.get_u8()? {
            0 => Ok(ImageWrap::Repeat),
            1 => Ok(ImageWrap::ClampToEdge),
            2 => Ok(ImageWrap::MirroredRepeat),
            _ => Err(PersistError::Malformed("unknown image wrap policy")),
        }
    };
    let sampler = ImageSampler {
        wrap_u: get_wrap(r)?,
        wrap_v: get_wrap(r)?,
    };
    let claimed_digest = r.get_digest()?;
    let encoded = r.get_bytes()?;
    budget.charge(encoded.len().saturating_mul(2), "image pixel resource")?;
    let mut pixels = Vec::new();
    budget.reserve(&mut pixels, encoded.len(), "image pixel resource")?;
    pixels.extend_from_slice(encoded);
    let image = ImageResource::rgba8(width, height, pixels, color_space, sampler)
        .map_err(|_| PersistError::Malformed("invalid image resource layout"))?;
    if image.content_digest() != claimed_digest {
        return Err(PersistError::Malformed("image content digest mismatch"));
    }
    Ok(image)
}

fn put_buffer(w: &mut Writer, buffer: &RecordBuffer) -> Result<(), SerialError> {
    let schema = buffer.schema();
    let fields = schema.fields();
    w.put_u16(count_u16(fields.len())?);
    for field in fields {
        w.put_str(&field.name).put_u16(count_u16(field.width)?);
    }
    let put_names = |w: &mut Writer, names: &[String]| -> Result<(), SerialError> {
        w.put_u16(count_u16(names.len())?);
        for name in names {
            w.put_str(name);
        }
        Ok(())
    };
    put_names(w, schema.aligned_keys())?;
    put_names(w, schema.pointlike_keys())?;
    w.put_u32(count_u32(buffer.len())?);
    for field in fields {
        if let Some(column) = buffer.read_column(&field.name) {
            for value in column {
                w.put_f32(value);
            }
        }
    }
    let locked = buffer.locked_keys();
    w.put_u16(count_u16(locked.len())?);
    for name in locked {
        w.put_str(name);
    }
    // Minor 1.7: the `_data_defaults` phantom record. Appended, never
    // interleaved — a pre-1.7 reader of this stream refuses under Strict
    // (NewerMinor), and this reader defaults older streams to no row.
    match buffer.defaults_row() {
        Some(row) => {
            w.put_bool(true);
            for &lane in row {
                w.put_f32(lane);
            }
        }
        None => {
            w.put_bool(false);
        }
    }
    Ok(())
}

fn get_names<'a>(
    r: &mut Reader<'a>,
    budget: &mut DecodeBudget,
    what: &'static str,
) -> Result<Vec<&'a str>, PersistError> {
    let count = r.get_u16()? as usize;
    // The borrowed name table itself is staging storage. The caller charges
    // owned names separately, before the schema or locked-key set copies them.
    preflight_count(r, budget, count, 8, std::mem::size_of::<&str>(), what)?;
    let mut names = Vec::new();
    budget.reserve(&mut names, count, what)?;
    for _ in 0..count {
        names.push(r.get_str()?);
    }
    Ok(names)
}

/// The current record layout is shared by arena snapshots and content-only
/// checkpoints. Older arena documents omit only the final defaults row.
fn get_buffer(
    r: &mut Reader<'_>,
    budget: &mut DecodeBudget,
    has_defaults_section: bool,
) -> Result<RecordBuffer, PersistError> {
    let n_fields = r.get_u16()? as usize;
    preflight_count(
        r,
        budget,
        n_fields,
        10,
        std::mem::size_of::<(&str, usize)>(),
        "record field table",
    )?;
    let mut fields = Vec::new();
    budget.reserve(&mut fields, n_fields, "record field table")?;
    for _ in 0..n_fields {
        let name = r.get_str()?;
        let width = r.get_u16()? as usize;
        if width == 0 {
            return Err(PersistError::Malformed("zero-width record field"));
        }
        fields.push((name, width));
    }
    let aligned = get_names(r, budget, "aligned key names")?;
    let pointlike = get_names(r, budget, "pointlike key names")?;
    let len = r.get_u32()? as usize;
    // RecordSchema owns its names. Charge both its tables and their strings
    // before construction rather than allocating names before budget checks.
    budget.charge(
        fields
            .len()
            .saturating_mul(std::mem::size_of::<crate::record::FieldSpec>())
            .saturating_add(
                aligned
                    .len()
                    .saturating_add(pointlike.len())
                    .saturating_mul(std::mem::size_of::<String>()),
            ),
        "record schema tables",
    )?;
    for name in fields
        .iter()
        .map(|(name, _)| *name)
        .chain(aligned.iter().copied())
        .chain(pointlike.iter().copied())
    {
        budget.charge(name.len(), "record schema names")?;
    }
    let schema = RecordSchema::new(&fields, &aligned, &pointlike)
        .map_err(|_| PersistError::Malformed("record schema stride overflows usize"))?;
    let stride = schema.stride();
    preflight_record_payload(r, len, stride, budget)?;
    // The storage keeps revision, view and dirty-span tables for each field.
    budget.charge(
        n_fields.saturating_mul(
            std::mem::size_of::<std::sync::atomic::AtomicU64>()
                + std::mem::size_of::<std::sync::atomic::AtomicUsize>()
                + std::mem::size_of::<std::sync::atomic::AtomicBool>()
                + std::mem::size_of::<Option<(usize, usize)>>(),
        ),
        "record field state",
    )?;
    let mut buffer = RecordBuffer::new(schema, len)
        .map_err(|_| PersistError::Malformed("record buffer sizing overflows usize"))?;
    for (name, width) in &fields {
        // The complete len * stride payload was proved to fit above.
        let lanes = len * width;
        budget.charge(
            lanes.saturating_mul(std::mem::size_of::<f32>()),
            "record column staging",
        )?;
        let mut column = Vec::new();
        budget.reserve(&mut column, lanes, "record column staging")?;
        for _ in 0..lanes {
            column.push(r.get_f32()?);
        }
        buffer.write_range(name, 0, &column);
    }
    let locked = get_names(r, budget, "locked key names")?;
    if !locked.is_empty() {
        // The set stores field indices, not copies of the encoded names.
        // Charge spare hash-table capacity and its occupancy bytes as well.
        budget.charge(
            locked
                .len()
                .saturating_mul(2 * (std::mem::size_of::<usize>() + 1)),
            "locked key indexes",
        )?;
        buffer.lock_data(locked);
    }
    if has_defaults_section && r.get_bool()? {
        preflight_record_payload(r, 1, stride, budget)?;
        budget.charge(
            stride.saturating_mul(std::mem::size_of::<f32>()),
            "record defaults staging",
        )?;
        let mut row = Vec::new();
        budget.reserve(&mut row, stride, "record defaults staging")?;
        for _ in 0..stride {
            row.push(r.get_f32()?);
        }
        buffer.restore_defaults_row(&row);
    }
    Ok(buffer)
}

// Before 1.9, the built-in dot renderer used a per-record f32 glow lane.
// Preserve the records themselves (including Python's exact dtype contract),
// but migrate the material only when one scalar represents every old point.
fn legacy_dot_glow(buffer: &RecordBuffer) -> Result<f64, PersistError> {
    match buffer.schema().field_width("glow_factor") {
        None => return Ok(0.0),
        Some(1) => {}
        Some(_) => {
            return Err(PersistError::Malformed(
                "legacy DotCloud glow_factor must have one lane",
            ));
        }
    }
    let Some(value) = buffer
        .constant_scalar_field("glow_factor")
        .filter(|value| value.is_finite())
    else {
        return Err(PersistError::Malformed(
            "legacy DotCloud per-point glow cannot be represented by one finite glow_factor uniform",
        ));
    };
    Ok(f64::from(value))
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
    w.put_u8(match u.joint_type {
        JointType::NoJoint => 0,
        JointType::Auto => 1,
        JointType::Bevel => 2,
        JointType::Miter => 3,
    });
    w.put_bool(u.flat_stroke)
        .put_bool(u.scale_stroke_with_zoom)
        .put_bool(u.stroke_behind)
        .put_bool(u.depth_test)
        .put_bool(u.use_winding_fill);
}

fn get_uniforms(r: &mut Reader<'_>) -> Result<Uniforms, PersistError> {
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
    let joint_type = match r.get_u8()? {
        0 => JointType::NoJoint,
        1 => JointType::Auto,
        2 => JointType::Bevel,
        3 => JointType::Miter,
        _ => return Err(PersistError::Malformed("unknown joint type")),
    };
    Ok(Uniforms {
        is_fixed_in_frame,
        shading,
        clip_planes,
        anti_alias_width,
        glow_factor: 0.0,
        joint_type,
        flat_stroke: r.get_bool()?,
        scale_stroke_with_zoom: r.get_bool()?,
        stroke_behind: r.get_bool()?,
        depth_test: r.get_bool()?,
        use_winding_fill: r.get_bool()?,
    })
}

fn put_entry(w: &mut Writer, entry: &SnapshotEntry) -> Result<(), SerialError> {
    put_buffer(w, &entry.buffer)?;
    w.put_u32(count_u32(entry.submobjects.len())?);
    for &m in &entry.submobjects {
        put_mob(w, m);
    }
    w.put_u32(count_u32(entry.parents.len())?);
    for &m in &entry.parents {
        put_mob(w, m);
    }
    // Updaters: identity + kind only — the honesty clause.
    w.put_u32(count_u32(entry.updaters.len())?);
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
    w.put_u64(count_u64(entry.pins)?)
        .put_bool(entry.pending_delete);
    put_uniforms(w, &entry.uniforms);
    put_shape(w, &entry.shape)?;
    // Minor 1.2 appends the §8.5 scene-sort key. Appended, never
    // interleaved: a 1.1 reader stops at the shape tag and is still
    // correct, and a 1.2 reader of a 1.1 stream defaults it to zero (the
    // Reference's default) — the §6.7 additive-minor rule.
    w.put_i32(entry.z_index);
    Ok(())
}

/// The semantic shape tag (§10.8), added in schema minor 1.1.
///
/// The tag carries durable class configuration — `Line`'s `path_arc`
/// outlives every transform — so dropping it on a round trip would change
/// meaning, not just performance. Encoded as a discriminant plus its
/// payload, followed by whether its geometry hint is live (`0` for live,
/// `u64::MAX` for absent). The process-local RecordBuffer revision number is
/// deliberately not durable semantic state; decode rebinds a live marker to
/// the reconstructed buffer's current `point` revision.
fn put_shape(w: &mut Writer, slot: &ShapeSlot) -> Result<(), SerialError> {
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
            w.put_u64(count_u64(vertices)?).put_bool(closed);
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
    w.put_u64(if slot.point_revision.is_some() {
        0
    } else {
        u64::MAX
    });
    Ok(())
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
            vertices: usize::try_from(r.get_u64()?)
                .map_err(|_| PersistError::Malformed("polyline vertex count overflows"))?,
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

fn preflight_record_payload(
    r: &Reader<'_>,
    len: usize,
    stride: usize,
    budget: &mut DecodeBudget,
) -> Result<(), PersistError> {
    let needed = len
        .checked_mul(stride)
        .and_then(|lanes| lanes.checked_mul(std::mem::size_of::<f32>()))
        .ok_or(PersistError::Serial(SerialError::SizeLimit {
            limit: Limits::DEFAULT.max_total,
            needed: usize::MAX,
        }))?;
    if needed > Limits::DEFAULT.max_total {
        return Err(PersistError::Serial(SerialError::SizeLimit {
            limit: Limits::DEFAULT.max_total,
            needed,
        }));
    }
    if needed > r.remaining() {
        return Err(PersistError::Serial(SerialError::UnexpectedEof {
            need: needed,
            remaining: r.remaining(),
        }));
    }
    // The destination RecordBuffer's cells decode at 1:1 against the
    // payload just proved feasible; charge them before construction.
    budget.charge(needed, "record buffer cells")
}

fn live_snapshot_entry(slots: &[(u32, Option<SnapshotEntry>)], mob: Mob) -> Option<&SnapshotEntry> {
    let (index, generation) = mob.parts();
    let (stored_generation, entry) = slots.get(index as usize)?;
    if *stored_generation != generation {
        return None;
    }
    entry.as_ref()
}

fn links_are_unique(links: &[Mob]) -> bool {
    let mut identities: Vec<u64> = links.iter().map(|mob| mob.bits()).collect();
    identities.sort_unstable();
    identities.windows(2).all(|pair| pair[0] != pair[1])
}

/// Validate the arena invariants that every public [`Stage`] mutation
/// preserves before the decoded state can reach [`Stage::restore`].
///
/// This pass is deliberately iterative: persisted family depth is input, not
/// a reason to consume the process stack. Its auxiliary storage is linear in
/// the already-decoded slot and edge inventories.
fn validate_decoded_arena(
    snapshot: &Snapshot,
    budget: &mut DecodeBudget,
) -> Result<(), PersistError> {
    // The validation pass's transient storage (the free-slot bitmap, the
    // indegree table, the traversal stack) is linear in the slot count;
    // charge it like every other destination structure (fm-vek.7).
    budget.charge(
        snapshot
            .slots
            .len()
            .saturating_mul(1 + std::mem::size_of::<u32>() + std::mem::size_of::<usize>()),
        "arena validation indexes",
    )?;
    let mut free_seen = vec![false; snapshot.slots.len()];
    for &raw_index in &snapshot.free {
        let index = raw_index as usize;
        let Some((_, entry)) = snapshot.slots.get(index) else {
            return Err(PersistError::Malformed("free slot index is out of range"));
        };
        if entry.is_some() {
            return Err(PersistError::Malformed(
                "free slot ledger names a live slot",
            ));
        }
        if std::mem::replace(&mut free_seen[index], true) {
            return Err(PersistError::Malformed("free slot ledger repeats an index"));
        }
    }
    if snapshot
        .slots
        .iter()
        .enumerate()
        .any(|(index, (_, entry))| entry.is_none() && !free_seen[index])
    {
        return Err(PersistError::Malformed(
            "free slot ledger omits an empty slot",
        ));
    }

    if snapshot
        .roots
        .iter()
        .any(|&root| live_snapshot_entry(&snapshot.slots, root).is_none())
    {
        return Err(PersistError::Malformed("scene root is not live"));
    }

    // Once liveness has proved each handle's generation, slot-index pairs
    // uniquely identify edges. Keeping the lookup in `(u32, u32)` form uses
    // exactly the same eight bytes as one serialized handle rather than
    // doubling auxiliary storage with two `u64` identities.
    let mut reverse_edges: Vec<(u32, u32)> = Vec::new();
    let mut child_edge_count = 0_usize;
    let mut live_count = 0_usize;
    for (slot_index, (_, entry)) in snapshot.slots.iter().enumerate() {
        let Some(entry) = entry else {
            continue;
        };
        live_count += 1;
        if entry.pending_delete && entry.pins == 0 {
            return Err(PersistError::Malformed("pending delete has no pins"));
        }
        if !links_are_unique(&entry.submobjects) || !links_are_unique(&entry.parents) {
            return Err(PersistError::Malformed("family edge is duplicated"));
        }
        if entry
            .submobjects
            .iter()
            .chain(&entry.parents)
            .any(|&mob| live_snapshot_entry(&snapshot.slots, mob).is_none())
        {
            return Err(PersistError::Malformed("family link is not live"));
        }
        // The reverse-edge index is linear in the parent-link inventory;
        // charge and reserve it fallibly before extending (fm-vek.7).
        budget.charge(
            entry
                .parents
                .len()
                .saturating_mul(std::mem::size_of::<(u32, u32)>()),
            "family reverse-edge index",
        )?;
        budget.reserve(
            &mut reverse_edges,
            entry.parents.len(),
            "family reverse-edge index",
        )?;
        child_edge_count = child_edge_count
            .checked_add(entry.submobjects.len())
            .ok_or(PersistError::Malformed("family edge count overflows"))?;
        let slot_index = u32::try_from(slot_index)
            .map_err(|_| PersistError::Malformed("arena slot count exceeds format"))?;
        reverse_edges.extend(
            entry
                .parents
                .iter()
                .map(|parent| (parent.parts().0, slot_index)),
        );
    }
    if child_edge_count != reverse_edges.len() {
        return Err(PersistError::Malformed(
            "family parent-child links are asymmetric",
        ));
    }
    reverse_edges.sort_unstable();
    for (slot_index, (_, entry)) in snapshot.slots.iter().enumerate() {
        let Some(entry) = entry else {
            continue;
        };
        let slot_index = u32::try_from(slot_index)
            .map_err(|_| PersistError::Malformed("arena slot count exceeds format"))?;
        if entry.submobjects.iter().any(|child| {
            reverse_edges
                .binary_search(&(slot_index, child.parts().0))
                .is_err()
        }) {
            return Err(PersistError::Malformed(
                "family parent-child links are asymmetric",
            ));
        }
    }

    let mut indegrees = vec![0_u32; snapshot.slots.len()];
    let mut ready = Vec::new();
    for (index, (_, entry)) in snapshot.slots.iter().enumerate() {
        let Some(entry) = entry else {
            continue;
        };
        indegrees[index] = u32::try_from(entry.parents.len())
            .map_err(|_| PersistError::Malformed("family parent count exceeds format"))?;
        if indegrees[index] == 0 {
            ready.push(index);
        }
    }
    let mut visited = 0_usize;
    while let Some(index) = ready.pop() {
        visited += 1;
        let entry = snapshot.slots[index]
            .1
            .as_ref()
            .ok_or(PersistError::Malformed(
                "family traversal reached a free slot",
            ))?;
        for child in &entry.submobjects {
            let (child_index, _) = child.parts();
            let degree = &mut indegrees[child_index as usize];
            *degree = degree.checked_sub(1).ok_or(PersistError::Malformed(
                "family parent-child links are asymmetric",
            ))?;
            if *degree == 0 {
                ready.push(child_index as usize);
            }
        }
    }
    if visited != live_count {
        return Err(PersistError::Malformed("family graph contains a cycle"));
    }
    Ok(())
}

impl Snapshot {
    /// Serialize into the versioned canonical container.
    ///
    /// # Errors
    /// [`SerialError::SizeLimit`] when the state exceeds
    /// [`Limits::DEFAULT`] or one of the format's fixed-width count fields.
    pub fn to_bytes(&self) -> Result<Vec<u8>, SerialError> {
        self.to_bytes_with_limits(Limits::DEFAULT)
    }

    /// Serialize with a document/field budget checked before writer growth.
    ///
    /// The canonical schema and bytes are identical to [`Self::to_bytes`].
    /// Explicit budgets can only narrow the production encoder's limits.
    ///
    /// # Errors
    /// [`SerialError::SizeLimit`] when the next field would exceed either
    /// effective budget or one of the format's fixed-width count fields.
    pub fn to_bytes_with_limits(&self, limits: Limits) -> Result<Vec<u8>, SerialError> {
        let limits = Limits {
            max_total: limits.max_total.min(Limits::DEFAULT.max_total),
            max_field: limits.max_field.min(Limits::DEFAULT.max_field),
        };
        let has_sorted_surface = self.slots.iter().any(|(_, entry)| {
            entry.as_ref().is_some_and(|entry| {
                matches!(
                    entry.render_primitive,
                    RenderPrimitive::CameraSortedSurfaceGrid { .. }
                )
            })
        });
        let has_padding_sources = self.slots.iter().any(|(_, entry)| {
            entry
                .as_ref()
                .is_some_and(|entry| entry.padding_source.is_some())
        });
        let has_glow_table = has_padding_sources
            || has_sorted_surface
            || self.slots.iter().any(|(_, entry)| {
                entry.as_ref().is_some_and(|entry| {
                    entry.uniforms.glow_factor != 0.0
                        || entry.render_primitive == RenderPrimitive::DotCloud
                })
            });
        let schema = if has_padding_sources {
            SNAPSHOT_SCHEMA
        } else if has_sorted_surface {
            SNAPSHOT_SCHEMA_WITH_SORTED_SURFACE
        } else if has_glow_table {
            SNAPSHOT_SCHEMA_WITH_GLOW
        } else {
            SNAPSHOT_SCHEMA_WITHOUT_GLOW
        };
        let mut w = Writer::with_limits(schema, limits);
        w.put_u32(count_u32(self.slots.len())?);
        for (generation, entry) in &self.slots {
            w.put_u32(*generation);
            match entry {
                Some(e) => {
                    w.put_bool(true);
                    put_entry(&mut w, e)?;
                }
                None => {
                    w.put_bool(false);
                }
            }
        }
        w.put_u32(count_u32(self.free.len())?);
        for &index in &self.free {
            w.put_u32(index);
        }
        w.put_u32(count_u32(self.roots.len())?);
        for &root in &self.roots {
            put_mob(&mut w, root);
        }
        w.put_u64(self.next_updater_id);
        // Minor 1.4: appended after the complete 1.3 payload so an older
        // compatible reader can stop before it. Runtime revision counters are
        // intentionally absent: canonical bytes describe state, not the edit
        // sequence that produced it.
        for (_, entry) in &self.slots {
            match entry {
                Some(entry) => {
                    w.put_bool(true);
                    for coefficient in entry.placement.coefficients() {
                        w.put_f64(coefficient);
                    }
                }
                None => {
                    w.put_bool(false);
                }
            }
        }
        // Minor 1.5: renderer program/topology identity is semantic state,
        // not a cache hint. Keep it in a liveness-parallel table so the 1.4
        // payload remains an exact prefix.
        for (_, entry) in &self.slots {
            match entry {
                Some(entry) => {
                    w.put_bool(true);
                    put_render_primitive(&mut w, entry.render_primitive)?;
                }
                None => {
                    w.put_bool(false);
                }
            }
        }
        // Minor 1.6: immutable image resources are semantic state. Their
        // descriptor and content digest precede the bytes so corruption and
        // descriptor drift are refused during decode.
        for (_, entry) in &self.slots {
            match entry {
                Some(entry) => {
                    w.put_bool(true);
                    match &entry.image {
                        Some(image) => {
                            w.put_bool(true);
                            put_image_resource(&mut w, image);
                        }
                        None => {
                            w.put_bool(false);
                        }
                    }
                }
                None => {
                    w.put_bool(false);
                }
            }
        }
        // Minor 1.8: a dark texture is part of the same immutable material,
        // not an ambient file path or a renderer-only cache. Preserve the old
        // primary-image table verbatim and append a liveness-parallel table.
        for (_, entry) in &self.slots {
            match entry {
                Some(entry) => {
                    w.put_bool(true);
                    let dark = entry.image.as_ref().and_then(ImageResource::dark_image);
                    w.put_bool(dark.is_some());
                    if let Some(dark) = dark {
                        put_image_resource(&mut w, dark);
                    }
                }
                None => {
                    w.put_bool(false);
                }
            }
        }
        // Minor 1.9 leaves the complete 1.8 payload intact. The table is also
        // emitted for a user-defined dot column named glow_factor: its bytes
        // remain ordinary record data, never a legacy material override.
        if has_glow_table {
            for (_, entry) in &self.slots {
                w.put_bool(entry.is_some());
                if let Some(entry) = entry {
                    w.put_f64(entry.uniforms.glow_factor);
                }
            }
        }
        // Minor 1.11 preserves the complete 1.10 payload and appends plain
        // provenance links. Like target/saved_state, a source can be stale;
        // recording its original generation must never alias a recycled slot.
        if has_padding_sources {
            for (_, entry) in &self.slots {
                w.put_bool(entry.is_some());
                if let Some(entry) = entry {
                    put_mob_opt(&mut w, entry.padding_source);
                }
            }
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
    /// (payload invariants), [`PersistError::AllocationLimit`] (the
    /// [`SnapshotLimits::DEFAULT`] decoded-allocation budget).
    pub fn from_bytes(bytes: &[u8], stage: &Stage) -> Result<DecodedSnapshot, PersistError> {
        Self::from_bytes_with_limits(bytes, stage, SnapshotLimits::DEFAULT)
    }

    /// [`Snapshot::from_bytes`] under an explicit decoded-allocation
    /// budget (fm-vek.7): every fixed-width count is preflighted against
    /// both the remaining encoded bytes and `limits` before the
    /// destination storage it names is reserved.
    ///
    /// # Errors
    /// As [`Snapshot::from_bytes`].
    pub fn from_bytes_with_limits(
        bytes: &[u8],
        stage: &Stage,
        limits: SnapshotLimits,
    ) -> Result<DecodedSnapshot, PersistError> {
        let stage_id = stage.stage_id();
        let mut budget = DecodeBudget::new(limits.max_total_decoded_bytes);
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
        // Each slot costs at least five encoded bytes (generation + live
        // flag); a count the payload cannot carry is refused here, before
        // the slot table it names is reserved (fm-vek.7).
        preflight_count(
            &r,
            &mut budget,
            slot_count,
            5,
            std::mem::size_of::<(u32, Option<SnapshotEntry>)>() + SnapshotEntry::ALLOCATION_BYTES,
            "arena slot table",
        )?;
        let mut slots: Vec<(u32, Option<SnapshotEntry>)> = Vec::new();
        budget.reserve(&mut slots, slot_count, "arena slot table")?;
        let mut manifest = UpdaterManifest::default();
        for slot_index in 0..slot_count {
            let generation = r.get_u32()?;
            let entry = if r.get_bool()? {
                let has_defaults_section = r.version().1 >= 7;
                let buffer = get_buffer(&mut r, &mut budget, has_defaults_section)?;
                // --- graph + state
                let n_sub = r.get_u32()? as usize;
                preflight_count(
                    &r,
                    &mut budget,
                    n_sub,
                    8,
                    std::mem::size_of::<Mob>(),
                    "family submobject links",
                )?;
                let mut submobjects = Vec::new();
                budget.reserve(&mut submobjects, n_sub, "family submobject links")?;
                for _ in 0..n_sub {
                    submobjects.push(get_mob(&mut r)?);
                }
                let n_par = r.get_u32()? as usize;
                preflight_count(
                    &r,
                    &mut budget,
                    n_par,
                    8,
                    std::mem::size_of::<Mob>(),
                    "family parent links",
                )?;
                let mut parents = Vec::new();
                budget.reserve(&mut parents, n_par, "family parent links")?;
                for _ in 0..n_par {
                    parents.push(get_mob(&mut r)?);
                }
                let n_upd = r.get_u32()? as usize;
                // u64 identity + u8 kind per updater on the wire.
                preflight_count(
                    &r,
                    &mut budget,
                    n_upd,
                    9,
                    std::mem::size_of::<(u64, UpdaterKindTag)>(),
                    "updater identities",
                )?;
                let mut ids = Vec::new();
                budget.reserve(&mut ids, n_upd, "updater identities")?;
                for _ in 0..n_upd {
                    let id = r.get_u64()?;
                    if id == 0 {
                        return Err(PersistError::Malformed(
                            "updater manifest carries the reserved zero identity",
                        ));
                    }
                    if ids.iter().any(|(existing, _)| *existing == id) {
                        return Err(PersistError::Malformed(
                            "updater manifest repeats an identity on one mobject",
                        ));
                    }
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
                let uniforms = get_uniforms(&mut r)?;
                // Schema minor 1.1 appended the shape tag; a 1.0 stream
                // simply has no shape, which is what General means.
                let mut shape = if r.version().1 >= 1 {
                    get_shape(&mut r)?
                } else {
                    ShapeSlot::default()
                };
                if shape.point_revision.is_some() {
                    shape.point_revision = buffer.field_revision("point");
                }
                // Minor 1.2 appended the scene-sort key (§8.5).
                let z_index = if r.version().1 >= 2 { r.get_i32()? } else { 0 };
                Some(SnapshotEntry::new(SnapshotEntryData {
                    buffer,
                    placement: Placement::IDENTITY,
                    placement_revision: 0,
                    submobjects: submobjects.into(),
                    parents: parents.into(),
                    updaters: crate::stage::RcVec::new(), // callables never serialize
                    updater_states: crate::stage::RcVec::new(), // replay owns executable state
                    updating_suspended,
                    is_animating,
                    tracker,
                    target,
                    saved_state,
                    // Legacy snapshots have no provenance. The 1.11 table
                    // below restores exact source identities when present.
                    padding_source: None,
                    pins,
                    pending_delete,
                    uniforms,
                    z_index,
                    shape,
                    render_primitive: RenderPrimitive::Vector,
                    image: None,
                    image_revision: 0,
                    family_cache: None,
                }))
            } else {
                None
            };
            slots.push((generation, entry));
        }
        let n_free = r.get_u32()? as usize;
        preflight_count(
            &r,
            &mut budget,
            n_free,
            4,
            std::mem::size_of::<u32>(),
            "free slot ledger",
        )?;
        let mut free = Vec::new();
        budget.reserve(&mut free, n_free, "free slot ledger")?;
        for _ in 0..n_free {
            free.push(r.get_u32()?);
        }
        let n_roots = r.get_u32()? as usize;
        preflight_count(
            &r,
            &mut budget,
            n_roots,
            8,
            std::mem::size_of::<Mob>(),
            "scene root table",
        )?;
        let mut roots = Vec::new();
        budget.reserve(&mut roots, n_roots, "scene root table")?;
        for _ in 0..n_roots {
            roots.push(get_mob(&mut r)?);
        }
        let max_updater_id = manifest
            .entries
            .iter()
            .flat_map(|(_, updaters)| updaters.iter().map(|(id, _)| *id))
            .max();
        // Minor 1.3 made the cursor part of durable state. Deriving from the
        // active manifest for older streams is the only compatible choice,
        // but 1.3 also preserves ids of updaters removed before the barrier.
        let next_updater_id = if r.version().1 >= 3 {
            let next = r.get_u64()?;
            if next == 0 || max_updater_id.is_some_and(|id| id >= next) {
                return Err(PersistError::Malformed(
                    "updater id cursor does not follow active identities",
                ));
            }
            next
        } else {
            max_updater_id
                .unwrap_or(0)
                .checked_add(1)
                .ok_or(PersistError::Malformed("updater id space is exhausted"))?
                .max(1)
        };
        if r.version().1 >= 4 {
            for entry in &mut slots {
                let present = r.get_bool()?;
                match (present, &mut entry.1) {
                    (false, None) => {}
                    (true, Some(entry)) => {
                        let coefficients = [
                            r.get_f64()?,
                            r.get_f64()?,
                            r.get_f64()?,
                            r.get_f64()?,
                            r.get_f64()?,
                            r.get_f64()?,
                            r.get_f64()?,
                            r.get_f64()?,
                            r.get_f64()?,
                            r.get_f64()?,
                            r.get_f64()?,
                            r.get_f64()?,
                        ];
                        entry.placement = Placement::new(
                            [
                                [coefficients[0], coefficients[1], coefficients[2]],
                                [coefficients[4], coefficients[5], coefficients[6]],
                                [coefficients[8], coefficients[9], coefficients[10]],
                            ],
                            [coefficients[3], coefficients[7], coefficients[11]],
                        );
                    }
                    _ => {
                        return Err(PersistError::Malformed(
                            "placement table does not match arena liveness",
                        ));
                    }
                }
            }
        }
        if r.version().1 >= 5 {
            for entry in &mut slots {
                let present = r.get_bool()?;
                match (present, &mut entry.1) {
                    (false, None) => {}
                    (true, Some(entry)) => {
                        let allow_sorted_surface = r.version().1 >= 10;
                        entry.render_primitive =
                            get_render_primitive(&mut r, allow_sorted_surface)?;
                    }
                    _ => {
                        return Err(PersistError::Malformed(
                            "render-primitive table does not match arena liveness",
                        ));
                    }
                }
            }
        }
        if r.version().1 >= 6 {
            for entry in &mut slots {
                let present = r.get_bool()?;
                match (present, &mut entry.1) {
                    (false, None) => {}
                    (true, Some(entry)) => {
                        if r.get_bool()? {
                            entry.image = Some(get_image_resource(&mut r, &mut budget)?);
                            entry.image_revision = 1;
                        }
                    }
                    _ => {
                        return Err(PersistError::Malformed(
                            "image-resource table does not match arena liveness",
                        ));
                    }
                }
            }
        }
        if r.version().1 >= 8 {
            for (_, entry) in &mut slots {
                let present = r.get_bool()?;
                match (present, entry) {
                    (false, None) => {}
                    (true, Some(entry)) => {
                        if r.get_bool()? {
                            budget.charge(
                                std::mem::size_of::<ImageResource>(),
                                "dark image descriptor",
                            )?;
                            let dark = get_image_resource(&mut r, &mut budget)?;
                            let light = entry.image.take().ok_or(PersistError::Malformed(
                                "dark image requires a primary image resource",
                            ))?;
                            entry.image = Some(light.with_dark_image(dark).map_err(|_| {
                                PersistError::Malformed("invalid light/dark image pair")
                            })?);
                        }
                    }
                    _ => {
                        return Err(PersistError::Malformed(
                            "dark-image table does not match arena liveness",
                        ));
                    }
                }
            }
        }
        if r.version().1 >= 9 {
            for (_, entry) in &mut slots {
                let present = r.get_bool()?;
                match (present, entry) {
                    (false, None) => {}
                    (true, Some(entry)) => entry.uniforms.glow_factor = r.get_f64()?,
                    _ => {
                        return Err(PersistError::Malformed(
                            "glow-uniform table does not match arena liveness",
                        ));
                    }
                }
            }
        } else {
            for (_, entry) in &mut slots {
                if let Some(entry) = entry
                    && entry.render_primitive == RenderPrimitive::DotCloud
                {
                    entry.uniforms.glow_factor = legacy_dot_glow(&entry.buffer)?;
                }
            }
        }
        if r.version().1 >= 11 {
            for (_, entry) in &mut slots {
                let present = r.get_bool()?;
                match (present, entry) {
                    (false, None) => {}
                    (true, Some(entry)) => {
                        // This is a plain link, not a family edge. Deleted
                        // sources remain stale after reminting, just like
                        // target/saved_state; their slot may now be reused.
                        // Its Option<Mob> storage was charged with the entry.
                        entry.padding_source = get_mob_opt(&mut r)?;
                    }
                    _ => {
                        return Err(PersistError::Malformed(
                            "padding-source table does not match arena liveness",
                        ));
                    }
                }
            }
        }
        r.finish()?;
        let snapshot = Snapshot {
            stage_id,
            next_updater_id,
            // The durable format does not carry the frame: a stage restoring
            // this keeps its own, and a materialized arena starts at the
            // default 16:9 frame (fm-8azr).
            frame_shape: [
                fmn_core::constants::FRAME_WIDTH,
                fmn_core::constants::FRAME_HEIGHT,
            ],
            slots,
            free: free.into(),
            roots: roots.into(),
        };
        validate_decoded_arena(&snapshot, &mut budget)?;
        Ok(DecodedSnapshot {
            snapshot,
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

// --------------------------------------------- content-only checkpoints

fn put_checkpoint_entry(w: &mut Writer, entry: &FamilyCheckpointEntry) -> Result<(), SerialError> {
    put_buffer(w, &entry.buffer)?;
    for coefficient in entry.placement.coefficients() {
        w.put_f64(coefficient);
    }
    w.put_u32(count_u32(entry.submobjects.len())?);
    for &child in &entry.submobjects {
        put_mob(w, child);
    }
    w.put_bool(entry.updating_suspended)
        .put_bool(entry.is_animating);
    w.put_bool(entry.tracker.is_some());
    if let Some(tracker) = entry.tracker {
        w.put_u8(match tracker.kind {
            crate::dynamics::TrackerKind::Plain => 0,
            crate::dynamics::TrackerKind::Exponential => 1,
            crate::dynamics::TrackerKind::Complex => 2,
        });
        w.put_f64(tracker.lanes[0]).put_f64(tracker.lanes[1]);
    }
    put_uniforms(w, &entry.uniforms);
    w.put_f64(entry.uniforms.glow_factor);
    put_shape(w, &entry.shape)?;
    w.put_i32(entry.z_index);
    put_render_primitive(w, entry.render_primitive)?;
    w.put_bool(entry.image.is_some());
    if let Some(image) = &entry.image {
        put_image_resource(w, image);
        let dark = image.dark_image();
        w.put_bool(dark.is_some());
        if let Some(dark) = dark {
            put_image_resource(w, dark);
        }
    }
    Ok(())
}

fn get_checkpoint_entry(
    r: &mut Reader<'_>,
    stage: &Stage,
    budget: &mut DecodeBudget,
) -> Result<FamilyCheckpointEntry, PersistError> {
    let buffer = get_buffer(r, budget, true)?;
    let mut coefficients = [0.0; 12];
    for coefficient in &mut coefficients {
        *coefficient = r.get_f64()?;
    }
    let placement = Placement::new(
        [
            [coefficients[0], coefficients[1], coefficients[2]],
            [coefficients[4], coefficients[5], coefficients[6]],
            [coefficients[8], coefficients[9], coefficients[10]],
        ],
        [coefficients[3], coefficients[7], coefficients[11]],
    );
    let count = r.get_u32()? as usize;
    preflight_count(
        r,
        budget,
        count,
        8,
        std::mem::size_of::<Mob>(),
        "checkpoint child links",
    )?;
    let mut submobjects = Vec::new();
    budget.reserve(&mut submobjects, count, "checkpoint child links")?;
    for _ in 0..count {
        submobjects.push(Mob::from_parts(
            stage.stage_id(),
            r.get_u32()?,
            r.get_u32()?,
        ));
    }
    if count != 0 {
        budget.charge(
            std::mem::size_of::<Vec<Mob>>() + 2 * std::mem::size_of::<usize>(),
            "checkpoint child sharing",
        )?;
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
        Some(crate::dynamics::Tracker {
            kind,
            lanes: [r.get_f64()?, r.get_f64()?],
        })
    } else {
        None
    };
    let mut uniforms = get_uniforms(r)?;
    uniforms.glow_factor = r.get_f64()?;
    let mut shape = get_shape(r)?;
    shape.point_revision = match shape.point_revision {
        None => None,
        Some(0) => Some(
            buffer
                .field_revision("point")
                .ok_or(PersistError::Malformed(
                    "checkpoint shape hint has no point field",
                ))?,
        ),
        Some(_) => {
            return Err(PersistError::Malformed(
                "invalid checkpoint shape hint marker",
            ));
        }
    };
    let z_index = r.get_i32()?;
    let render_primitive = get_render_primitive(r, true)?;
    let image = if r.get_bool()? {
        let mut image = get_image_resource(r, budget)?;
        if r.get_bool()? {
            budget.charge(
                std::mem::size_of::<ImageResource>(),
                "dark image descriptor",
            )?;
            let dark = get_image_resource(r, budget)?;
            image = image
                .with_dark_image(dark)
                .map_err(|_| PersistError::Malformed("invalid light/dark image pair"))?;
        }
        Some(image)
    } else {
        None
    };
    Ok(FamilyCheckpointEntry {
        buffer,
        placement,
        submobjects: submobjects.into(),
        updating_suspended,
        is_animating,
        tracker,
        uniforms,
        z_index,
        shape,
        render_primitive,
        image,
    })
}

/// The same descendant-closed, acyclic, depth-first graph produced by
/// Stage::family, checked without allocating a slot-index-sized arena or
/// consuming call-stack space proportional to an untrusted family depth.
fn validate_checkpoint_graph(
    checkpoint: &FamilyCheckpoint,
    budget: &mut DecodeBudget,
) -> Result<(), PersistError> {
    let count = checkpoint.entries.len();
    if count == 0 {
        return Err(PersistError::Malformed("empty family checkpoint"));
    }
    budget.charge(
        count.saturating_mul(
            std::mem::size_of::<(u64, usize)>()
                + std::mem::size_of::<usize>()
                + std::mem::size_of::<u8>()
                + std::mem::size_of::<(usize, usize)>(),
        ),
        "checkpoint graph validation",
    )?;
    let mut identities = Vec::new();
    budget.reserve(&mut identities, count, "checkpoint identity index")?;
    identities.extend(
        checkpoint
            .entries
            .iter()
            .enumerate()
            .map(|(index, (member, _))| (member.bits(), index)),
    );
    identities.sort_unstable();
    if identities.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(PersistError::Malformed("checkpoint repeats a member"));
    }
    let index_of = |member: Mob| -> Result<usize, PersistError> {
        identities
            .binary_search_by_key(&member.bits(), |(bits, _)| *bits)
            .map(|index| identities[index].1)
            .map_err(|_| PersistError::Malformed("checkpoint child is outside its family"))
    };
    let mut last_parent = Vec::new();
    budget.reserve(&mut last_parent, count, "checkpoint edge uniqueness")?;
    last_parent.resize(count, usize::MAX);
    for (parent_index, (_, entry)) in checkpoint.entries.iter().enumerate() {
        for &child in &entry.submobjects {
            let child_index = index_of(child)?;
            if last_parent[child_index] == parent_index {
                return Err(PersistError::Malformed("checkpoint repeats a child edge"));
            }
            last_parent[child_index] = parent_index;
        }
    }
    let mut colors = Vec::new();
    budget.reserve(&mut colors, count, "checkpoint traversal colors")?;
    colors.resize(count, 0_u8);
    let mut stack = Vec::new();
    budget.reserve(&mut stack, count, "checkpoint traversal stack")?;
    colors[0] = 1;
    stack.push((0_usize, 0_usize));
    let mut next_member = 1;
    while let Some((index, next_child)) = stack.last_mut() {
        let children = &checkpoint.entries[*index].1.submobjects;
        let Some(&child) = children.get(*next_child) else {
            colors[*index] = 2;
            stack.pop();
            continue;
        };
        *next_child += 1;
        let child_index = index_of(child)?;
        match colors[child_index] {
            1 => {
                return Err(PersistError::Malformed(
                    "checkpoint family contains a cycle",
                ));
            }
            2 => {}
            _ => {
                if child_index != next_member {
                    return Err(PersistError::Malformed(
                        "checkpoint members are not in family order",
                    ));
                }
                next_member += 1;
                colors[child_index] = 1;
                stack.push((child_index, 0));
            }
        }
    }
    if next_member != count {
        return Err(PersistError::Malformed(
            "checkpoint has unreachable members",
        ));
    }
    Ok(())
}

impl FamilyCheckpoint {
    /// Encode the captured family under [`FAMILY_CHECKPOINT_SCHEMA`].
    ///
    /// Only captured content is read. The live Stage may have advanced or
    /// rearranged the family since capture. Updater registrations and native
    /// executable state are neither serialized nor inferred from that content.
    ///
    /// # Errors
    /// [`SerialError::SizeLimit`] if the document or a fixed-width count
    /// exceeds the production format limits.
    pub fn to_bytes(&self) -> Result<Vec<u8>, SerialError> {
        self.to_bytes_with_limits(Limits::DEFAULT)
    }

    /// Encode with a document/field budget checked before writer growth.
    /// Explicit limits can only narrow the production limits; admitted bytes
    /// are identical to [`Self::to_bytes`].
    ///
    /// # Errors
    /// As [`Self::to_bytes`], under the narrower budget.
    pub fn to_bytes_with_limits(&self, limits: Limits) -> Result<Vec<u8>, SerialError> {
        let limits = Limits {
            max_total: limits.max_total.min(Limits::DEFAULT.max_total),
            max_field: limits.max_field.min(Limits::DEFAULT.max_field),
        };
        let mut writer = Writer::with_limits(FAMILY_CHECKPOINT_SCHEMA, limits);
        writer.put_u32(count_u32(self.entries.len())?);
        for (member, entry) in &self.entries {
            put_mob(&mut writer, *member);
            put_checkpoint_entry(&mut writer, entry)?;
        }
        writer.finish()
    }

    /// Decode saved family content against an already restored live Stage.
    ///
    /// Each serialized slot/generation adopts `stage`'s logical identity and
    /// must resolve there. Saved child edges need not match today's layout,
    /// but must form one complete acyclic family. No mobjects are allocated,
    /// callbacks invoked, or live content changed. Call
    /// [`Stage::restore_family_checkpoint`] separately to restore the content;
    /// callers still own admission and rebinding of executable animation state.
    ///
    /// # Errors
    /// [`PersistError`] for a corrupt/unsupported container, a stale member,
    /// invalid graph, or exceeded decoded-allocation budget.
    pub fn from_bytes(bytes: &[u8], stage: &Stage) -> Result<Self, PersistError> {
        Self::from_bytes_with_limits(bytes, stage, SnapshotLimits::DEFAULT)
    }

    /// [`Self::from_bytes`] with an explicit aggregate decoded-allocation
    /// budget, including record/image data and graph-validation indexes.
    /// Every encoded count is preflighted before its storage is reserved.
    ///
    /// # Errors
    /// As [`Self::from_bytes`].
    pub fn from_bytes_with_limits(
        bytes: &[u8],
        stage: &Stage,
        limits: SnapshotLimits,
    ) -> Result<Self, PersistError> {
        let mut reader = Reader::open(
            bytes,
            FAMILY_CHECKPOINT_SCHEMA,
            Limits::DEFAULT,
            UnknownPolicy::Strict,
        )?;
        let count = reader.get_u32()? as usize;
        if count == 0 {
            return Err(PersistError::Malformed("empty family checkpoint"));
        }
        let mut budget = DecodeBudget::new(limits.max_total_decoded_bytes);
        preflight_count(
            &reader,
            &mut budget,
            count,
            8,
            std::mem::size_of::<(Mob, FamilyCheckpointEntry)>(),
            "checkpoint member table",
        )?;
        let mut entries = Vec::new();
        budget.reserve(&mut entries, count, "checkpoint member table")?;
        for _ in 0..count {
            let member = stage
                .resolve_snapshot_handle(reader.get_u32()?, reader.get_u32()?)
                .ok_or(PersistError::Malformed("checkpoint member is not live"))?;
            entries.push((
                member,
                get_checkpoint_entry(&mut reader, stage, &mut budget)?,
            ));
        }
        reader.finish()?;
        let checkpoint = Self { entries };
        validate_checkpoint_graph(&checkpoint, &mut budget)?;
        Ok(checkpoint)
    }
}

// ----------------------------------------------------------- SceneState

/// The §13.1 scene-scope state: exact rational-clock position, play count,
/// the one RNG's state words (fmn-core's export surface), and the arena
/// snapshot. The scene runtime (fm-5xm) captures and re-applies it; this
/// layer owns the bytes.
pub struct SceneState {
    /// Exact number of elapsed frames at capture.
    pub frames_elapsed: i64,
    /// Exact frame grid at capture.
    pub fps: u32,
    /// Completed `play()` count at capture.
    pub play_count: u64,
    /// `Pcg64Dxsm::state()`: `((state_hi, state_lo), (inc_hi, inc_lo))`.
    pub rng_state: ([u64; 2], [u64; 2]),
    /// The arena.
    pub snapshot: Snapshot,
}

/// A decoded scene state: the fields plus the snapshot's updater manifest.
pub struct DecodedSceneState {
    /// Exact number of elapsed frames at capture.
    pub frames_elapsed: i64,
    /// Exact frame grid at capture.
    pub fps: u32,
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

struct SceneStateEnvelope<'a> {
    frames_elapsed: i64,
    fps: u32,
    play_count: u64,
    rng_state: ([u64; 2], [u64; 2]),
    snapshot_bytes: &'a [u8],
}

impl<'a> SceneStateEnvelope<'a> {
    fn read(bytes: &'a [u8]) -> Result<Self, PersistError> {
        let mut r = Reader::open(
            bytes,
            SCENE_STATE_SCHEMA,
            Limits::DEFAULT,
            UnknownPolicy::Strict,
        )?;
        let frames_elapsed = r.get_i64()?;
        let fps = r.get_u32()?;
        let play_count = r.get_u64()?;
        let state = [r.get_u64()?, r.get_u64()?];
        let inc = [r.get_u64()?, r.get_u64()?];
        let snapshot_bytes = r.get_bytes()?;
        r.finish()?;
        Ok(Self {
            frames_elapsed,
            fps,
            play_count,
            rng_state: (state, inc),
            snapshot_bytes,
        })
    }
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
    /// Capture the scene-scope state from a stage, its exact clock position,
    /// a play counter, and the RNG.
    #[must_use]
    pub fn capture(
        stage: &Stage,
        frames_elapsed: i64,
        fps: u32,
        play_count: u64,
        rng: &Pcg64Dxsm,
    ) -> Self {
        Self {
            frames_elapsed,
            fps,
            play_count,
            rng_state: rng.state(),
            snapshot: stage.snapshot(),
        }
    }

    /// Serialize the envelope (exact clock fields, play count, RNG words,
    /// then the nested snapshot document as a length-prefixed field).
    ///
    /// # Errors
    /// As [`Snapshot::to_bytes`].
    pub fn to_bytes(&self) -> Result<Vec<u8>, SerialError> {
        let snapshot_bytes = self.snapshot.to_bytes()?;
        let mut w = Writer::new(SCENE_STATE_SCHEMA);
        w.put_i64(self.frames_elapsed)
            .put_u32(self.fps)
            .put_u64(self.play_count);
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
        let envelope = SceneStateEnvelope::read(bytes)?;
        // The nested snapshot document is borrowed straight out of the
        // envelope — no transit clone (fm-vek.7); its own decode re-opens
        // it as a container and budgets what it materializes.
        let decoded = Snapshot::from_bytes(envelope.snapshot_bytes, stage)?;
        Ok(DecodedSceneState {
            frames_elapsed: envelope.frames_elapsed,
            fps: envelope.fps,
            play_count: envelope.play_count,
            rng_state: envelope.rng_state,
            snapshot: decoded.snapshot,
            updaters: decoded.updaters,
        })
    }

    /// Read the exact clock from a checksum-verified scene envelope without
    /// materializing its nested arena or updater manifest.
    ///
    /// This validates the envelope only, **not** the nested snapshot. It is
    /// useful for bounded cold reconstruction that subsequently compares the
    /// entire input document with independently captured canonical state.
    /// Installing decoded state still requires [`Self::from_bytes`].
    ///
    /// # Errors
    /// [`PersistError`] for a malformed, oversized or corrupt envelope.
    pub fn frame_clock_from_bytes(bytes: &[u8]) -> Result<(i64, u32), PersistError> {
        let envelope = SceneStateEnvelope::read(bytes)?;
        Ok((envelope.frames_elapsed, envelope.fps))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mobject;

    fn checkpoint_diamond() -> (Stage, Mob, Mob) {
        let mut stage = Stage::new();
        let root = stage.add(Mobject::new());
        let left = stage.add(Mobject::new());
        let right = stage.add(Mobject::new());
        let shared = stage.add(Mobject::from_points(&[[1.0, 2.0, 3.0]]));
        let outside = stage.add(Mobject::new());
        stage.attach(root, left).unwrap();
        stage.attach(root, right).unwrap();
        stage.attach(left, shared).unwrap();
        stage.attach(right, shared).unwrap();
        stage.attach(outside, shared).unwrap();
        (stage, root, outside)
    }

    fn assert_checkpoint_malformed(
        stage: &Stage,
        checkpoint: &FamilyCheckpoint,
        message: &'static str,
    ) {
        let before = stage.snapshot_bytes().unwrap();
        let epoch = stage.topology_epoch();
        let error = FamilyCheckpoint::from_bytes(&checkpoint.to_bytes().unwrap(), stage)
            .map(|_| ())
            .unwrap_err();
        assert_eq!(error, PersistError::Malformed(message));
        assert_eq!(stage.snapshot_bytes().unwrap(), before);
        assert_eq!(stage.topology_epoch(), epoch);
    }

    #[test]
    fn checkpoint_decode_refuses_incomplete_duplicated_cyclic_and_reordered_graphs() {
        let (stage, root, outside) = checkpoint_diamond();
        let mut empty = stage.checkpoint_family(root).unwrap();
        empty.entries.clear();
        assert_checkpoint_malformed(&stage, &empty, "empty family checkpoint");

        let mut duplicated = stage.checkpoint_family(root).unwrap();
        duplicated.entries[1].0 = root;
        assert_checkpoint_malformed(&stage, &duplicated, "checkpoint repeats a member");

        let mut repeated_edge = stage.checkpoint_family(root).unwrap();
        let first_child = repeated_edge.entries[1].0;
        repeated_edge.entries[0].1.submobjects.push(first_child);
        assert_checkpoint_malformed(&stage, &repeated_edge, "checkpoint repeats a child edge");

        let mut external_child = stage.checkpoint_family(root).unwrap();
        external_child.entries[0].1.submobjects.push(outside);
        assert_checkpoint_malformed(
            &stage,
            &external_child,
            "checkpoint child is outside its family",
        );

        let mut cyclic = stage.checkpoint_family(root).unwrap();
        cyclic.entries[2].1.submobjects.push(root);
        assert_checkpoint_malformed(&stage, &cyclic, "checkpoint family contains a cycle");

        let mut reordered = stage.checkpoint_family(root).unwrap();
        reordered.entries.swap(1, 3);
        assert_checkpoint_malformed(
            &stage,
            &reordered,
            "checkpoint members are not in family order",
        );

        let mut unreachable = stage.checkpoint_family(root).unwrap();
        // The outside parent's own capture includes the shared leaf already
        // present above. Include only that parent to isolate reachability.
        unreachable
            .entries
            .push(stage.checkpoint_family(outside).unwrap().entries.remove(0));
        assert_checkpoint_malformed(&stage, &unreachable, "checkpoint has unreachable members");
    }

    #[test]
    fn checkpoint_counts_refuse_before_allocating_record_or_child_tables() {
        let (stage, root, _) = checkpoint_diamond();
        let saved = stage.checkpoint_family(root).unwrap();
        let root_entry = &saved.entries[0].1;
        let mut fields = Writer::new(FAMILY_CHECKPOINT_SCHEMA);
        fields.put_u32(1);
        put_mob(&mut fields, root);
        fields.put_u16(u16::MAX);

        let mut children = Writer::new(FAMILY_CHECKPOINT_SCHEMA);
        children.put_u32(1);
        put_mob(&mut children, root);
        put_buffer(&mut children, &root_entry.buffer).unwrap();
        for coefficient in root_entry.placement.coefficients() {
            children.put_f64(coefficient);
        }
        children.put_u32(u32::MAX);
        for writer in [fields, children] {
            assert!(matches!(
                FamilyCheckpoint::from_bytes(&writer.finish().unwrap(), &stage),
                Err(PersistError::Serial(SerialError::UnexpectedEof { .. }))
            ));
        }
        assert!(matches!(
            FamilyCheckpoint::from_bytes_with_limits(
                &saved.to_bytes().unwrap(),
                &stage,
                SnapshotLimits {
                    max_total_decoded_bytes: 1
                },
            ),
            Err(PersistError::AllocationLimit {
                what: "checkpoint member table",
                ..
            })
        ));
    }

    #[test]
    fn checkpoint_validation_is_iterative_and_storage_depends_on_family_not_slot_number() {
        let mut stage = Stage::new();
        let mut entries = Vec::new();
        for _ in 0..4096 {
            let mob = stage.add(Mobject::new());
            entries.push(stage.checkpoint_family(mob).unwrap().entries.remove(0));
        }
        // A saved deep chain, with all its original members still live but
        // detached today. Decode must inspect history without replay or a
        // recursion depth proportional to the saved graph.
        for index in 0..entries.len() - 1 {
            let child = entries[index + 1].0;
            entries[index].1.submobjects.push(child);
        }
        let deep = FamilyCheckpoint { entries };
        let bytes = deep.to_bytes().unwrap();
        let decoded = FamilyCheckpoint::from_bytes(&bytes, &stage).unwrap();
        assert_eq!(decoded.entries.len(), 4096);
        assert_eq!(decoded.to_bytes().unwrap(), bytes);

        let last = stage.add(Mobject::new());
        let single = stage.checkpoint_family(last).unwrap().to_bytes().unwrap();
        assert!(single.len() < 1024);
        let decoded = FamilyCheckpoint::from_bytes_with_limits(
            &single,
            &stage,
            SnapshotLimits {
                max_total_decoded_bytes: 4096,
            },
        )
        .unwrap();
        assert_eq!(decoded.entries.len(), 1);
        assert_eq!(decoded.entries[0].0, last);
    }

    #[test]
    fn scene_clock_reader_checks_envelope_without_claiming_snapshot_validation() {
        let stage = Stage::new();
        let state = SceneState::capture(&stage, 17, 30, 4, &Pcg64Dxsm::from_seed(9));
        let bytes = state.to_bytes().unwrap();
        assert_eq!(
            SceneState::frame_clock_from_bytes(&bytes).unwrap(),
            (17, 30)
        );
        let decoded = SceneState::from_bytes(&bytes, &stage).unwrap();
        assert_eq!((decoded.frames_elapsed, decoded.fps), (17, 30));

        let mut writer = Writer::new(SCENE_STATE_SCHEMA);
        writer.put_i64(17).put_u32(30).put_u64(4);
        for word in [0, 0, 0, 1] {
            writer.put_u64(word);
        }
        writer.put_bytes(b"not a snapshot document");
        let mut malformed = writer.finish().unwrap();
        assert_eq!(
            SceneState::frame_clock_from_bytes(&malformed).unwrap(),
            (17, 30)
        );
        assert!(SceneState::from_bytes(&malformed, &stage).is_err());
        malformed[0] ^= 1;
        assert!(SceneState::frame_clock_from_bytes(&malformed).is_err());
        assert!(SceneState::frame_clock_from_bytes(&bytes[..bytes.len() - 1]).is_err());
    }

    fn entry_mut(snapshot: &mut Snapshot, mob: Mob) -> &mut SnapshotEntry {
        let (index, generation) = mob.parts();
        let (stored_generation, entry) = &mut snapshot.slots[index as usize];
        assert_eq!(*stored_generation, generation);
        entry.as_mut().expect("test handle must name a live entry")
    }

    fn assert_malformed(snapshot: &Snapshot, stage: &Stage, expected: &'static str) {
        let bytes = snapshot.to_bytes().unwrap();
        let error = Snapshot::from_bytes(&bytes, stage)
            .map(|_| ())
            .expect_err("the malformed canonical snapshot must be refused");
        assert_eq!(error, PersistError::Malformed(expected));
    }

    #[test]
    fn decode_refuses_impossible_lifetime_and_free_slot_ledgers() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::new());

        let mut pending_without_pins = stage.snapshot();
        entry_mut(&mut pending_without_pins, mob).pending_delete = true;
        assert_malformed(&pending_without_pins, &stage, "pending delete has no pins");

        let mut out_of_range = stage.snapshot();
        out_of_range
            .free
            .push(u32::try_from(out_of_range.slots.len()).unwrap());
        assert_malformed(&out_of_range, &stage, "free slot index is out of range");

        let mut live_slot = stage.snapshot();
        live_slot.free.push(0);
        assert_malformed(&live_slot, &stage, "free slot ledger names a live slot");

        let free_mob = stage.add(Mobject::new());
        stage.delete(free_mob).unwrap();
        let mut duplicate = stage.snapshot();
        duplicate.free.push(duplicate.free[0]);
        assert_malformed(&duplicate, &stage, "free slot ledger repeats an index");

        let mut missing = stage.snapshot();
        missing.free.clear();
        assert_malformed(&missing, &stage, "free slot ledger omits an empty slot");
    }

    #[test]
    fn decode_refuses_impossible_roots_and_family_graphs() {
        let mut stage = Stage::new();
        let parent = stage.add(Mobject::new());
        let child = stage.add(Mobject::new());

        let mut stale_root = stage.snapshot();
        let (index, generation) = parent.parts();
        stale_root.roots.push(Mob::from_parts(
            stale_root.stage_id,
            index,
            generation.wrapping_add(1),
        ));
        assert_malformed(&stale_root, &stage, "scene root is not live");

        let mut stale_edge = stage.snapshot();
        let (index, generation) = child.parts();
        let stale_child = Mob::from_parts(stale_edge.stage_id, index, generation.wrapping_add(1));
        entry_mut(&mut stale_edge, parent)
            .submobjects
            .push(stale_child);
        assert_malformed(&stale_edge, &stage, "family link is not live");

        let mut duplicate_edge = stage.snapshot();
        entry_mut(&mut duplicate_edge, parent)
            .submobjects
            .extend([child, child]);
        entry_mut(&mut duplicate_edge, child).parents.push(parent);
        assert_malformed(&duplicate_edge, &stage, "family edge is duplicated");

        let mut asymmetric = stage.snapshot();
        entry_mut(&mut asymmetric, parent).submobjects.push(child);
        assert_malformed(
            &asymmetric,
            &stage,
            "family parent-child links are asymmetric",
        );

        let mut cyclic = stage.snapshot();
        entry_mut(&mut cyclic, parent).submobjects.push(child);
        entry_mut(&mut cyclic, parent).parents.push(child);
        entry_mut(&mut cyclic, child).submobjects.push(parent);
        entry_mut(&mut cyclic, child).parents.push(parent);
        assert_malformed(&cyclic, &stage, "family graph contains a cycle");
    }

    #[test]
    fn decode_accepts_runtime_reachable_lifetime_and_graph_states() {
        let mut stage = Stage::new();
        let root = stage.add(Mobject::new());
        let left = stage.add(Mobject::new());
        let right = stage.add(Mobject::new());
        let shared = stage.add(Mobject::new());
        stage.attach(root, left).unwrap();
        stage.attach(root, right).unwrap();
        stage.attach(left, shared).unwrap();
        stage.attach(right, shared).unwrap();
        stage.add_many_to_scene(&[root, root]).unwrap();

        stage.pin(left).unwrap();
        stage.delete(left).unwrap();

        stage.generate_target(right).unwrap();
        let target = stage.target(right).unwrap();
        stage.delete(target).unwrap();
        stage.save_state(right).unwrap();
        let saved = stage.saved_state(right).unwrap();
        stage.delete(saved).unwrap();

        let bytes = stage.snapshot_bytes().unwrap();
        let decoded = Snapshot::from_bytes(&bytes, &stage).unwrap();
        let mut snapshot = decoded.snapshot;
        assert_eq!(snapshot.roots, [root, root]);
        assert_eq!(snapshot.to_bytes().unwrap(), bytes);
        let left_entry = entry_mut(&mut snapshot, left);
        assert_eq!(left_entry.pins, 1);
        assert!(left_entry.pending_delete);
    }

    #[test]
    fn decode_validates_a_deep_family_without_recursion() {
        const DEPTH: usize = 4_096;

        let mut stage = Stage::new();
        let handles: Vec<Mob> = (0..DEPTH).map(|_| stage.add(Mobject::new())).collect();
        let mut snapshot = stage.snapshot();
        for edge in handles.windows(2) {
            entry_mut(&mut snapshot, edge[0]).submobjects.push(edge[1]);
            entry_mut(&mut snapshot, edge[1]).parents.push(edge[0]);
        }

        let bytes = snapshot.to_bytes().unwrap();
        let decoded = Snapshot::from_bytes(&bytes, &stage).unwrap();
        assert_eq!(decoded.snapshot.slots.len(), DEPTH);
    }
    #[test]
    fn defaults_row_round_trips_and_is_adopted_on_growth() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::new());
        let field = stage.get(mob).unwrap().buffer.schema().fields()[0]
            .name
            .clone();
        let width = stage
            .get(mob)
            .unwrap()
            .buffer
            .schema()
            .field_width(&field)
            .expect("first schema field has a width");
        let values: Vec<f32> = (0..width).map(|i| 1.0 + 0.5 * i as f32).collect();
        stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .write_default_record(&field, &values);
        assert!(stage.get(mob).unwrap().buffer.has_defaults());

        let bytes = stage.snapshot().to_bytes().unwrap();
        let mut decoded = Snapshot::from_bytes(&bytes, &stage).unwrap();
        let restored = entry_mut(&mut decoded.snapshot, mob);
        assert!(restored.buffer.has_defaults());
        let row = restored.buffer.defaults_row().unwrap();
        assert_eq!(&row[..values.len()], &values[..]);
        assert!(row[values.len()..].iter().all(|lane| *lane == 0.0));

        // The whole point of the row: growth from zero adopts it.
        let mut grown = restored.buffer.deep_clone();
        grown.resize(2).unwrap();
        let expected: Vec<f32> = values
            .iter()
            .copied()
            .chain(values.iter().copied())
            .collect();
        assert_eq!(grown.read_column(&field).unwrap(), expected);
    }

    #[test]
    fn buffers_without_defaults_round_trip_without_the_section() {
        let mut stage = Stage::new();
        let empty = stage.add(Mobject::new());
        let filled = stage.add(Mobject::new());
        let field = stage.get(filled).unwrap().buffer.schema().fields()[0]
            .name
            .clone();
        let width = stage
            .get(filled)
            .unwrap()
            .buffer
            .schema()
            .field_width(&field)
            .expect("first schema field has a width");
        stage
            .get_mut(filled)
            .unwrap()
            .buffer
            .resize(2)
            .expect("growth within allocation limits");
        let column = vec![0.25; 2 * width];
        stage
            .get_mut(filled)
            .unwrap()
            .buffer
            .write_range(&field, 0, &column);

        assert!(!stage.get(empty).unwrap().buffer.has_defaults());
        assert!(!stage.get(filled).unwrap().buffer.has_defaults());

        let bytes = stage.snapshot().to_bytes().unwrap();
        let mut decoded = Snapshot::from_bytes(&bytes, &stage).unwrap();
        assert!(
            !entry_mut(&mut decoded.snapshot, empty)
                .buffer
                .has_defaults()
        );
        assert!(
            !entry_mut(&mut decoded.snapshot, filled)
                .buffer
                .has_defaults()
        );
    }
}
