//! The replay journal + effect model (§13.4, §18, R16, fm-y7u):
//! record everything, replay cheaply, invalidate conservatively.
//!
//! The journal is the one record with three consumers:
//!
//! 1. **The supervisor's edit-replay** (W9SUPER, fm-39s): restore the
//!    nearest checkpoint at or before the last valid entry and replay
//!    from there. Entries whose recorded identities and asset hashes
//!    still match are trusted; a changed callback hash invalidates
//!    from exactly that point ([`plan_replay`]).
//! 2. **The purity classifier's evidence** (§9.5, R20): the effect
//!    model *is* the classifier's vocabulary — [`EffectClass`] embeds
//!    the recorded [`fmn_anim::purity`] classification per segment, so
//!    frame-parallel eligibility is journaled, auditable state.
//! 3. **The pipeline's synchronization vocabulary** (§17.4): an entry
//!    classed [`EffectClass::PixelObserving`] is a pipeline barrier
//!    that drains in-flight frames. Ordinary manim scene code never
//!    produces one — asserted on the command corpus in tests.
//!
//! **The soundness doctrine (R16).** Replay-cache unsoundness is the
//! failure mode, and the design answer is conservative invalidation
//! everywhere: a [`CommandKind::Custom`] command is coerced to
//! [`EffectClass::Opaque`] no matter what its recorder claimed; any
//! entry that touched a subprocess is a replay barrier; a divergent
//! state hash mid-replay ([`ReplayAudit`]) falls back to full
//! re-execution and records why. When in doubt, a barrier.
//!
//! Also here: the one-command **repro bundle** (§18) — journal + input
//! closure, content-addressed, so every bug report is a deterministic
//! replay — and the journal's content hash for provenance sidecars.

use fmn_anim::purity::{ImpureEffect, Purity};
use fmn_hash::serial::{Error as SerialError, Limits, Reader, Schema, UnknownPolicy, Writer};
use fmn_hash::sha256::{Digest, sha256};

/// The journal's versioned container schema (FMNA/3).
pub const JOURNAL_SCHEMA: Schema = Schema::new(*b"FMNA", 3, 1, 0);
/// The repro bundle's versioned container schema (FMNA/4).
pub const BUNDLE_SCHEMA: Schema = Schema::new(*b"FMNA", 4, 1, 0);

/// A journal failure.
#[derive(Debug)]
pub enum JournalError {
    /// The canonical container refused the bytes.
    Serial(SerialError),
    /// The payload decoded but violates a journal invariant.
    Malformed(&'static str),
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serial(e) => write!(f, "journal container: {e}"),
            Self::Malformed(what) => write!(f, "malformed journal: {what}"),
        }
    }
}

impl std::error::Error for JournalError {}

impl From<SerialError> for JournalError {
    fn from(e: SerialError) -> Self {
        Self::Serial(e)
    }
}

/// Serializable mirror of the classifier's impurity vocabulary
/// ([`ImpureEffect`]) — the journal's on-disk form of R20 evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImpureEffectTag {
    /// A dt-updater somewhere in the frame's families.
    DtUpdater,
    /// A non-dt (scene) updater.
    SceneUpdater,
    /// An animation without a declared pure signature.
    UnclassifiedAnimation,
    /// A `wait_until` stop condition.
    StopCondition,
}

impl From<ImpureEffect> for ImpureEffectTag {
    fn from(e: ImpureEffect) -> Self {
        match e {
            ImpureEffect::DtUpdater => Self::DtUpdater,
            ImpureEffect::SceneUpdater => Self::SceneUpdater,
            ImpureEffect::UnclassifiedAnimation => Self::UnclassifiedAnimation,
            ImpureEffect::StopCondition => Self::StopCondition,
        }
    }
}

impl ImpureEffectTag {
    const fn code(self) -> u8 {
        match self {
            Self::DtUpdater => 0,
            Self::SceneUpdater => 1,
            Self::UnclassifiedAnimation => 2,
            Self::StopCondition => 3,
        }
    }

    fn from_code(code: u8) -> Result<Self, JournalError> {
        Ok(match code {
            0 => Self::DtUpdater,
            1 => Self::SceneUpdater,
            2 => Self::UnclassifiedAnimation,
            3 => Self::StopCondition,
            _ => return Err(JournalError::Malformed("impure effect tag")),
        })
    }
}

/// The journal-level effect model (§13.4) — what one command did to
/// the world, as far as replay and the pipeline are concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectClass {
    /// Pure per §9.5: frame-parallel eligible, freely replayable.
    Pure,
    /// Stateful with the recorded reasons: serial front-end, still
    /// replayable (its outcome is a function of journaled state).
    Stateful(Vec<ImpureEffectTag>),
    /// The operation observes rendered pixels: a §17.4 pipeline
    /// barrier (drains in-flight frames) and a replay barrier.
    PixelObserving,
    /// Unrecognized: the conservative default. A replay barrier.
    Opaque,
}

impl EffectClass {
    /// Lift a segment classification into the journal vocabulary.
    #[must_use]
    pub fn from_purity(purity: &Purity) -> Self {
        match purity {
            Purity::Pure => Self::Pure,
            Purity::Stateful(effects) => {
                Self::Stateful(effects.iter().map(|&e| e.into()).collect())
            }
        }
    }

    /// Whether the §17.4 pipeline must drain in-flight frames before
    /// this effect runs.
    #[must_use]
    pub const fn is_pipeline_barrier(&self) -> bool {
        matches!(self, Self::PixelObserving)
    }

    /// Whether replay must stop *before* this entry and re-execute it
    /// (conservative: anything that cannot be proven equivalent from
    /// the record).
    #[must_use]
    pub const fn is_replay_barrier(&self) -> bool {
        matches!(self, Self::PixelObserving | Self::Opaque)
    }
}

/// The operation classes the journal recognizes. Anything else is
/// [`CommandKind::Custom`] — and Custom is opaque by decree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    /// A `play()` segment.
    Play,
    /// A `wait()`/`wait_until()` segment.
    Wait,
    /// Scene membership addition.
    Add,
    /// Scene membership removal.
    Remove,
    /// A camera state change.
    CameraChange,
    /// An `add_sound` operation.
    Sound,
    /// Anything the vocabulary does not recognize.
    Custom,
}

impl CommandKind {
    const fn code(self) -> u8 {
        match self {
            Self::Play => 0,
            Self::Wait => 1,
            Self::Add => 2,
            Self::Remove => 3,
            Self::CameraChange => 4,
            Self::Sound => 5,
            Self::Custom => 6,
        }
    }

    fn from_code(code: u8) -> Result<Self, JournalError> {
        Ok(match code {
            0 => Self::Play,
            1 => Self::Wait,
            2 => Self::Add,
            3 => Self::Remove,
            4 => Self::CameraChange,
            5 => Self::Sound,
            6 => Self::Custom,
            _ => return Err(JournalError::Malformed("command kind")),
        })
    }
}

/// One command's identity: its kind, a canonical digest of everything
/// that determines its behavior (parameters, callback version hashes),
/// and a human label for the inspector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRecord {
    /// The operation class.
    pub kind: CommandKind,
    /// The identity digest — two commands with equal digests are
    /// behaviorally identical by construction of the digest.
    pub identity: Digest,
    /// Human-facing label (`"play FadeIn(circle)"`).
    pub label: String,
}

impl CommandRecord {
    /// Whether `other` re-executes to the same behavior.
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        self.kind == other.kind && self.identity == other.identity
    }
}

/// A content-addressed file/font/asset read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetRead {
    /// The path as the scene addressed it.
    pub path: String,
    /// SHA-256 of the bytes that were read.
    pub digest: Digest,
}

/// A journaled subprocess (ffmpeg) invocation — provenance, and a
/// replay barrier (side effects on disk cannot be proven equivalent
/// from the record).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubprocessRecord {
    /// SHA-256 of the tool's executable bytes, hex.
    pub tool_sha256_hex: String,
    /// Digest of the canonical argv.
    pub argv_digest: Digest,
    /// The published destination.
    pub destination: String,
}

/// One journal entry: a command, its effect, everything it read, and
/// the state it produced.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The command's identity.
    pub command: CommandRecord,
    /// The recorded effect class.
    pub effect: EffectClass,
    /// Content-addressed reads this command performed.
    pub reads: Vec<AssetRead>,
    /// Subprocess invocations this command performed.
    pub subprocesses: Vec<SubprocessRecord>,
    /// A full [`fmn_mobject::SceneState`] checkpoint (its canonical
    /// bytes) taken after this command, when the checkpoint policy
    /// took one. Carries the RNG state at the barrier by construction.
    pub checkpoint: Option<Vec<u8>>,
    /// SHA-256 of the post-command `SceneState` bytes — the divergence
    /// detector [`ReplayAudit`] compares against.
    pub state_hash: Digest,
}

impl Entry {
    /// Whether replay may not skip past this entry.
    #[must_use]
    pub fn is_replay_barrier(&self) -> bool {
        self.effect.is_replay_barrier() || !self.subprocesses.is_empty()
    }
}

/// The append-only journal.
#[derive(Debug, Clone, Default)]
pub struct Journal {
    entries: Vec<Entry>,
}

impl Journal {
    /// An empty journal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an entry, applying the conservative coercions: a
    /// [`CommandKind::Custom`] command's effect becomes
    /// [`EffectClass::Opaque`] regardless of what the recorder claimed
    /// (R16 — unrecognized operations do not get to describe
    /// themselves as replayable).
    pub fn record(&mut self, mut entry: Entry) {
        if entry.command.kind == CommandKind::Custom {
            entry.effect = EffectClass::Opaque;
        }
        self.entries.push(entry);
    }

    /// The recorded entries, in order.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Serialize into the versioned canonical container.
    ///
    /// # Errors
    /// [`SerialError`] on size-limit overflow.
    #[allow(clippy::cast_possible_truncation)]
    pub fn to_bytes(&self) -> Result<Vec<u8>, SerialError> {
        let mut w = Writer::new(JOURNAL_SCHEMA);
        w.put_u32(self.entries.len() as u32);
        for entry in &self.entries {
            put_command(&mut w, &entry.command);
            put_effect(&mut w, &entry.effect);
            w.put_u32(entry.reads.len() as u32);
            for read in &entry.reads {
                w.put_str(&read.path);
                w.put_digest(&read.digest);
            }
            w.put_u32(entry.subprocesses.len() as u32);
            for sub in &entry.subprocesses {
                w.put_str(&sub.tool_sha256_hex);
                w.put_digest(&sub.argv_digest);
                w.put_str(&sub.destination);
            }
            match &entry.checkpoint {
                Some(bytes) => {
                    w.put_bool(true);
                    w.put_bytes(bytes);
                }
                None => {
                    w.put_bool(false);
                }
            }
            w.put_digest(&entry.state_hash);
        }
        w.finish()
    }

    /// Decode a journal.
    ///
    /// # Errors
    /// [`JournalError`] on container or payload violations.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, JournalError> {
        let mut r = Reader::open(
            bytes,
            JOURNAL_SCHEMA,
            Limits::DEFAULT,
            UnknownPolicy::Strict,
        )?;
        let count = r.get_u32()? as usize;
        let mut entries = Vec::with_capacity(count.min(65_536));
        for _ in 0..count {
            let command = get_command(&mut r)?;
            let effect = get_effect(&mut r)?;
            let read_count = r.get_u32()? as usize;
            let mut reads = Vec::with_capacity(read_count.min(4096));
            for _ in 0..read_count {
                reads.push(AssetRead {
                    path: r.get_str()?.to_string(),
                    digest: r.get_digest()?,
                });
            }
            let sub_count = r.get_u32()? as usize;
            let mut subprocesses = Vec::with_capacity(sub_count.min(4096));
            for _ in 0..sub_count {
                subprocesses.push(SubprocessRecord {
                    tool_sha256_hex: r.get_str()?.to_string(),
                    argv_digest: r.get_digest()?,
                    destination: r.get_str()?.to_string(),
                });
            }
            let checkpoint = if r.get_bool()? {
                Some(r.get_bytes()?.to_vec())
            } else {
                None
            };
            let state_hash = r.get_digest()?;
            entries.push(Entry {
                command,
                effect,
                reads,
                subprocesses,
                checkpoint,
                state_hash,
            });
        }
        r.finish()?;
        Ok(Self { entries })
    }

    /// The journal's content address — what provenance sidecars carry.
    ///
    /// # Errors
    /// As [`Journal::to_bytes`].
    pub fn content_hash(&self) -> Result<Digest, SerialError> {
        Ok(sha256(&self.to_bytes()?))
    }
}

fn put_command(w: &mut Writer, command: &CommandRecord) {
    w.put_u8(command.kind.code());
    w.put_digest(&command.identity);
    w.put_str(&command.label);
}

fn get_command(r: &mut Reader<'_>) -> Result<CommandRecord, JournalError> {
    let kind = CommandKind::from_code(r.get_u8()?)?;
    let identity = r.get_digest()?;
    let label = r.get_str()?.to_string();
    Ok(CommandRecord {
        kind,
        identity,
        label,
    })
}

#[allow(clippy::cast_possible_truncation)]
fn put_effect(w: &mut Writer, effect: &EffectClass) {
    match effect {
        EffectClass::Pure => {
            w.put_u8(0);
        }
        EffectClass::Stateful(tags) => {
            w.put_u8(1);
            w.put_u32(tags.len() as u32);
            for tag in tags {
                w.put_u8(tag.code());
            }
        }
        EffectClass::PixelObserving => {
            w.put_u8(2);
        }
        EffectClass::Opaque => {
            w.put_u8(3);
        }
    }
}

fn get_effect(r: &mut Reader<'_>) -> Result<EffectClass, JournalError> {
    Ok(match r.get_u8()? {
        0 => EffectClass::Pure,
        1 => {
            let count = r.get_u32()? as usize;
            if count > 16 {
                return Err(JournalError::Malformed("impure effect count"));
            }
            let mut tags = Vec::with_capacity(count);
            for _ in 0..count {
                tags.push(ImpureEffectTag::from_code(r.get_u8()?)?);
            }
            EffectClass::Stateful(tags)
        }
        2 => EffectClass::PixelObserving,
        3 => EffectClass::Opaque,
        _ => return Err(JournalError::Malformed("effect class")),
    })
}

// ---- replay planning ------------------------------------------------

/// Why reuse stopped short of the full recorded journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidationReason {
    /// The incoming command at `index` differs from the recorded one
    /// (a changed callback hash lands here).
    CommandMismatch {
        /// The first divergent entry.
        index: usize,
    },
    /// A recorded asset read no longer verifies.
    AssetChanged {
        /// The entry whose read failed.
        index: usize,
        /// The changed asset's path.
        path: String,
    },
    /// The entry is a replay barrier (opaque, pixel-observing, or
    /// subprocess-touching): it must re-execute.
    ReplayBarrier {
        /// The barrier entry.
        index: usize,
    },
    /// The incoming stream ended before the recorded journal did.
    StreamExhausted {
        /// The first recorded entry with no incoming counterpart.
        index: usize,
    },
}

/// The replay plan: how much of the recorded journal to trust, and
/// where execution resumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayPlan {
    /// Entries `[0, reuse)` are verified reusable: their identities
    /// match, their asset reads still verify, and none is a barrier.
    pub reuse: usize,
    /// The latest entry index `< reuse` carrying a checkpoint —
    /// restore it, then re-execute commands after it (each verified
    /// equivalent by the record). `None`: cold start from the top.
    pub resume_checkpoint: Option<usize>,
    /// Why reuse stopped, when it stopped short of the whole journal.
    pub reason: Option<InvalidationReason>,
}

/// Plan a replay of `incoming` against the recorded journal.
///
/// The conservative walk: reuse grows only while the incoming command
/// matches the recorded identity, every recorded read still verifies
/// (`asset_ok`), and the entry is not a replay barrier. The first
/// failure stops the walk and is recorded as the reason — when in
/// doubt, a barrier (R16).
#[must_use]
pub fn plan_replay(
    journal: &Journal,
    incoming: &[CommandRecord],
    asset_ok: &dyn Fn(&AssetRead) -> bool,
) -> ReplayPlan {
    let mut reuse = 0usize;
    let mut reason = None;
    for (index, entry) in journal.entries().iter().enumerate() {
        let Some(command) = incoming.get(index) else {
            reason = Some(InvalidationReason::StreamExhausted { index });
            break;
        };
        if !entry.command.matches(command) {
            reason = Some(InvalidationReason::CommandMismatch { index });
            break;
        }
        if entry.is_replay_barrier() {
            reason = Some(InvalidationReason::ReplayBarrier { index });
            break;
        }
        if let Some(read) = entry.reads.iter().find(|read| !asset_ok(read)) {
            reason = Some(InvalidationReason::AssetChanged {
                index,
                path: read.path.clone(),
            });
            break;
        }
        reuse = index + 1;
    }
    let resume_checkpoint = journal.entries()[..reuse]
        .iter()
        .rposition(|entry| entry.checkpoint.is_some());
    ReplayPlan {
        reuse,
        resume_checkpoint,
        reason,
    }
}

/// The mid-replay divergence detector (R16's fallback clause): as the
/// supervisor re-executes verified entries, it feeds each produced
/// state hash here; the first mismatch flips the audit to diverged,
/// recording where — the caller then falls back to full re-execution
/// from its last good checkpoint, correctly and silently.
#[derive(Debug, Clone, Default)]
pub struct ReplayAudit {
    verified: usize,
    diverged_at: Option<usize>,
}

impl ReplayAudit {
    /// A fresh audit.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Check one replayed entry's produced state against the record.
    /// Returns whether replay may continue.
    pub fn step(&mut self, journal: &Journal, index: usize, produced: &Digest) -> bool {
        if self.diverged_at.is_some() {
            return false;
        }
        match journal.entries().get(index) {
            Some(entry) if entry.state_hash == *produced => {
                self.verified = index + 1;
                true
            }
            _ => {
                self.diverged_at = Some(index);
                false
            }
        }
    }

    /// Entries verified equivalent so far.
    #[must_use]
    pub const fn verified(&self) -> usize {
        self.verified
    }

    /// Where replay diverged, if it did — the recorded "why".
    #[must_use]
    pub const fn diverged_at(&self) -> Option<usize> {
        self.diverged_at
    }
}

// ---- the repro bundle (§18) ----------------------------------------

/// A bundle failed verification: the named divergent asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleDivergence {
    /// The divergent path.
    pub path: String,
    /// The digest the bundle recorded.
    pub expected: Digest,
    /// The digest found on this machine (`None`: unreadable/absent).
    pub found: Option<Digest>,
}

impl std::fmt::Display for BundleDivergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "repro bundle diverges at {:?}: expected {}, found {}",
            self.path,
            self.expected.to_hex(),
            self.found
                .as_ref()
                .map_or_else(|| "nothing readable".to_string(), Digest::to_hex),
        )
    }
}

/// The one-command repro bundle: the journal plus the content-hashed
/// input closure. A bug report carrying one is a deterministic replay.
#[derive(Debug, Clone)]
pub struct ReproBundle {
    /// The scene's human name.
    pub scene_label: String,
    /// The session seed.
    pub seed: u64,
    /// The exact rational frame rate.
    pub fps: (u32, u32),
    /// The input closure: every file the session read, content-hashed
    /// (scene sources, fonts, assets, config).
    pub closure: Vec<AssetRead>,
    /// The session journal.
    pub journal: Journal,
}

impl ReproBundle {
    /// Serialize into the versioned canonical container.
    ///
    /// # Errors
    /// [`SerialError`] on size-limit overflow.
    #[allow(clippy::cast_possible_truncation)]
    pub fn to_bytes(&self) -> Result<Vec<u8>, SerialError> {
        let mut w = Writer::new(BUNDLE_SCHEMA);
        w.put_str(&self.scene_label);
        w.put_u64(self.seed);
        w.put_u32(self.fps.0);
        w.put_u32(self.fps.1);
        w.put_u32(self.closure.len() as u32);
        for read in &self.closure {
            w.put_str(&read.path);
            w.put_digest(&read.digest);
        }
        w.put_bytes(&self.journal.to_bytes()?);
        w.finish()
    }

    /// Decode a bundle.
    ///
    /// # Errors
    /// [`JournalError`] on container or payload violations.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, JournalError> {
        let mut r = Reader::open(bytes, BUNDLE_SCHEMA, Limits::DEFAULT, UnknownPolicy::Strict)?;
        let scene_label = r.get_str()?.to_string();
        let seed = r.get_u64()?;
        let fps = (r.get_u32()?, r.get_u32()?);
        let count = r.get_u32()? as usize;
        let mut closure = Vec::with_capacity(count.min(65_536));
        for _ in 0..count {
            closure.push(AssetRead {
                path: r.get_str()?.to_string(),
                digest: r.get_digest()?,
            });
        }
        let journal = Journal::from_bytes(r.get_bytes()?)?;
        r.finish()?;
        Ok(Self {
            scene_label,
            seed,
            fps,
            closure,
            journal,
        })
    }

    /// The bundle's content address.
    ///
    /// # Errors
    /// As [`ReproBundle::to_bytes`].
    pub fn content_hash(&self) -> Result<Digest, SerialError> {
        Ok(sha256(&self.to_bytes()?))
    }

    /// Verify the closure on this machine: every recorded asset must
    /// read back to its recorded digest.
    ///
    /// # Errors
    /// The first [`BundleDivergence`], named.
    pub fn verify(&self, read: &dyn Fn(&str) -> Option<Vec<u8>>) -> Result<(), BundleDivergence> {
        for asset in &self.closure {
            let found = read(&asset.path).map(|bytes| sha256(&bytes));
            if found.as_ref() != Some(&asset.digest) {
                return Err(BundleDivergence {
                    path: asset.path.clone(),
                    expected: asset.digest,
                    found,
                });
            }
        }
        Ok(())
    }
}
