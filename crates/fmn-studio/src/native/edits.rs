//! Canonical, bounded input tracks. Executable/editor state is reconstructed,
//! never deserialized as closures or replaced with a second editing engine.

use fmn_hash::{Digest, Limits, Reader, Schema, UnknownPolicy, Writer};
use fmn_scene::{AssetRead, CommandKind, CommandRecord};

use crate::protocol::{StudioInput, studio_input_payload};
use crate::{ProtocolDigest, ServiceError};

use super::NativeSceneProgram;
use super::program::{
    NativeProgramCheckpoint, NativeWaitCursor, NativeWaitTailCursor, execution_error, invalid,
};

const EDIT_STATE_SCHEMA: Schema = Schema::new(*b"FMEI", 1, 2, 0);
const RESUME_STATE_SCHEMA: Schema = Schema::new(*b"FMNR", 1, 0, 0);
// Keep the all-wait record and its canonical bytes unchanged. A trailing-wait
// continuation additionally binds the completed executable prefix's counters.
const WAIT_TAIL_RESUME_STATE_SCHEMA: Schema = Schema::new(*b"FMNR", 2, 0, 0);
/// Hard ceiling independent of caller policy; each label is separately bounded.
pub(super) const MAX_RECORDED_INPUTS: usize = 65_536;

struct InputRow {
    command: CommandRecord,
    input: StudioInput,
}

#[derive(Default)]
pub(super) struct EditTrack {
    rows: Vec<InputRow>,
}

impl EditTrack {
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn try_clone(&self) -> Result<Self, ServiceError> {
        let mut rows = Vec::new();
        rows.try_reserve_exact(self.rows.len())
            .map_err(execution_error)?;
        for row in &self.rows {
            let mut label = String::new();
            label
                .try_reserve_exact(row.command.label.len())
                .map_err(execution_error)?;
            label.push_str(&row.command.label);
            rows.push(InputRow {
                command: CommandRecord {
                    kind: row.command.kind,
                    identity: row.command.identity,
                    label,
                },
                input: row.input.clone(),
            });
        }
        Ok(Self { rows })
    }

    /// A new edit in the past abandons only the later input branch. Existing
    /// events at this frame keep their serial order. Revision is journal length,
    /// not frame number, so even two same-frame clients cannot overwrite edits.
    pub fn append(
        &mut self,
        scene: &str,
        command: &CommandRecord,
        position: u64,
        limit: usize,
    ) -> Result<u64, ServiceError> {
        let input = studio_input_payload(scene, command).map_err(execution_error)?;
        if input.revision != position {
            return Err(invalid("stale native input revision"));
        }
        if input.target.is_some() {
            return Err(invalid(
                "stale native input object or unsupported explicit target",
            ));
        }
        let keep = self
            .rows
            .partition_point(|row| row.input.frame <= input.frame);
        if keep >= limit {
            return Err(invalid(
                "native committed input track exceeds its event budget",
            ));
        }
        let mut label = String::new();
        label
            .try_reserve_exact(command.label.len())
            .map_err(execution_error)?;
        label.push_str(&command.label);
        self.rows.try_reserve(1).map_err(execution_error)?;
        self.rows.truncate(keep);
        let frame = u64::try_from(input.frame).map_err(execution_error)?;
        self.rows.push(InputRow {
            command: CommandRecord {
                kind: command.kind,
                identity: command.identity,
                label,
            },
            input,
        });
        Ok(frame)
    }

    pub fn visible_count(&self, target: u64) -> u64 {
        self.rows
            .partition_point(|row| row.input.frame.cast_unsigned() <= target) as u64
    }

    /// Reuse the one native dispatcher and actual stepped execution boundary.
    /// None means a fresh frame-zero owner (including frame-zero inputs).
    /// Some(frame) means that capture's inputs have already been applied.
    pub fn advance(
        &self,
        program: &mut NativeSceneProgram,
        target: u64,
        after: Option<u64>,
    ) -> Result<(), ServiceError> {
        let applied = after.map_or(0, |frame| {
            self.rows
                .partition_point(|row| row.input.frame.cast_unsigned() <= frame)
        });
        self.advance_after_inputs(program, target, applied)
    }

    /// Continue a verified replay candidate whose first `applied` rows have
    /// already run. Unlike a frame cutoff, this keeps newly appended inputs at
    /// the current frame visible: selection, nudge and undo can share a capture
    /// while still being distinct ordered commands.
    ///
    /// The caller preserves the applied prefix, or reconstructs a fresh owner
    /// after moving backwards. Never reuse a cursor after replacing that prefix.
    pub fn advance_after_inputs(
        &self,
        program: &mut NativeSceneProgram,
        target: u64,
        applied: usize,
    ) -> Result<(), ServiceError> {
        let remaining = self
            .rows
            .get(applied..)
            .ok_or_else(|| invalid("native replay input cursor exceeds its track"))?;
        for row in remaining {
            let frame = row.input.frame.cast_unsigned();
            if frame > target {
                break;
            }
            program.advance_to(frame)?;
            program.dispatch(row.input.event.clone())?;
        }
        program.advance_to(target)
    }
}

/// Empty tracks keep the original SceneState wire format. An edited checkpoint
/// owns its full branch (including inputs after a backward scrub), the actual
/// SceneState at its capture, the input closure, and its exact journal position.
pub(super) fn encode_state(
    scene: &str,
    position: u64,
    reads: &[AssetRead],
    edits: &EditTrack,
    state: Vec<u8>,
    max_bytes: usize,
) -> Result<Vec<u8>, ServiceError> {
    if state.len() > max_bytes {
        return Err(execution_error(
            "native SceneState exceeds the checkpoint budget",
        ));
    }
    if edits.is_empty() {
        return Ok(state);
    }
    let mut writer = Writer::with_limits(
        EDIT_STATE_SCHEMA,
        Limits {
            max_total: max_bytes,
            max_field: max_bytes,
        },
    );
    writer
        .put_str(scene)
        .put_u64(position)
        .put_u32(reads.len() as u32);
    for read in reads {
        writer.put_str(&read.path).put_bytes(read.digest.as_bytes());
    }
    writer.put_u32(edits.rows.len() as u32);
    for row in &edits.rows {
        writer
            .put_bytes(row.command.identity.as_bytes())
            .put_str(&row.command.label);
    }
    writer.put_bytes(&state);
    writer.finish().map_err(execution_error)
}

pub(super) struct RestoredTrack<'a> {
    pub edits: EditTrack,
    pub state: &'a [u8],
}

pub(super) fn decode_state<'a>(
    bytes: &'a [u8],
    scene: &str,
    position: u64,
    reads: &[AssetRead],
    max_inputs: usize,
    frame_count: u64,
    max_bytes: usize,
) -> Result<RestoredTrack<'a>, ServiceError> {
    if bytes.len() > max_bytes {
        return Err(invalid("native checkpoint exceeds its byte budget"));
    }
    if !bytes.starts_with(b"FMEI") {
        // The caller still validates this through SceneState's strict reader.
        return Ok(RestoredTrack {
            edits: EditTrack::default(),
            state: bytes,
        });
    }
    let mut reader = Reader::open(
        bytes,
        EDIT_STATE_SCHEMA,
        Limits {
            max_total: max_bytes,
            max_field: max_bytes,
        },
        UnknownPolicy::Strict,
    )
    .map_err(execution_error)?;
    if reader.get_str().map_err(execution_error)? != scene
        || reader.get_u64().map_err(execution_error)? != position
    {
        return Err(invalid(
            "edited checkpoint scene or journal position differs",
        ));
    }
    if reader.get_u32().map_err(execution_error)? as usize != reads.len() {
        return Err(invalid("edited checkpoint input closure differs"));
    }
    for read in reads {
        if reader.get_str().map_err(execution_error)? != read.path
            || reader.get_bytes().map_err(execution_error)? != read.digest.as_bytes()
        {
            return Err(invalid("edited checkpoint input closure differs"));
        }
    }
    let count = reader.get_u32().map_err(execution_error)? as usize;
    if count == 0 || count > max_inputs || count > MAX_RECORDED_INPUTS || count > bytes.len() / 48 {
        return Err(invalid("edited checkpoint input count is invalid"));
    }
    let mut edits = EditTrack::default();
    edits
        .rows
        .try_reserve_exact(count)
        .map_err(execution_error)?;
    for _ in 0..count {
        let digest: [u8; 32] = reader
            .get_bytes()
            .map_err(execution_error)?
            .try_into()
            .map_err(execution_error)?;
        let label = reader.get_str().map_err(execution_error)?;
        if label.len() > crate::protocol::MAX_STUDIO_INPUT_LABEL_BYTES {
            return Err(invalid("edited checkpoint input label exceeds its budget"));
        }
        let mut owned = String::new();
        owned
            .try_reserve_exact(label.len())
            .map_err(execution_error)?;
        owned.push_str(label);
        let command = CommandRecord {
            kind: CommandKind::Input,
            identity: Digest::from_bytes(digest),
            label: owned,
        };
        let input = studio_input_payload(scene, &command).map_err(execution_error)?;
        if input.target.is_some()
            || input.revision >= position
            || input.frame.cast_unsigned() >= frame_count
            || edits.rows.last().is_some_and(|row| {
                row.input.frame > input.frame || row.input.revision >= input.revision
            })
        {
            return Err(invalid(
                "edited checkpoint input ordering or target is invalid",
            ));
        }
        edits.rows.push(InputRow { command, input });
    }
    let state = reader.get_bytes().map_err(execution_error)?;
    reader.finish().map_err(execution_error)?;
    Ok(RestoredTrack { edits, state })
}

/// An admitted continuation binds executable declarations separately from the
/// scene snapshot. The supervisor owns the expected digest; the envelope binds
/// that state to this exact build, input closure, factory and journal position.
pub(super) fn encode_resume_state(
    scene: &str,
    position: u64,
    reads: &[AssetRead],
    factory: ProtocolDigest,
    cursor: &NativeProgramCheckpoint,
    state: &[u8],
    max_bytes: usize,
) -> Result<Vec<u8>, ServiceError> {
    let schema = if cursor.tail.is_some() {
        WAIT_TAIL_RESUME_STATE_SCHEMA
    } else {
        RESUME_STATE_SCHEMA
    };
    let mut writer = Writer::with_limits(
        schema,
        Limits {
            max_total: max_bytes,
            max_field: max_bytes,
        },
    );
    writer
        .put_str(scene)
        .put_u64(position)
        .put_u32(reads.len() as u32);
    for read in reads {
        writer.put_str(&read.path).put_bytes(read.digest.as_bytes());
    }
    writer
        .put_bytes(factory.as_bytes())
        .put_u64(cursor.frame)
        .put_u64(cursor.frame_shape[0])
        .put_u64(cursor.frame_shape[1])
        .put_u64(cursor.started_segments as u64)
        .put_u8(cursor.effects);
    if let Some(tail) = &cursor.tail {
        writer
            .put_u64(tail.index as u64)
            .put_i64(tail.prefix_frames)
            .put_u64(tail.prefix_segments as u64)
            .put_u64(tail.prefix_plays);
    }
    writer.put_bool(cursor.active.is_some());
    if let Some(wait) = &cursor.active {
        writer
            .put_i64(wait.base_frame)
            .put_i64(wait.stepped)
            .put_u8(wait.purity);
    }
    writer.put_bytes(state);
    writer.finish().map_err(execution_error)
}

pub(super) struct RestoredContinuation<'a> {
    pub cursor: NativeProgramCheckpoint,
    pub state: &'a [u8],
}

pub(super) fn decode_resume_state<'a>(
    bytes: &'a [u8],
    scene: &str,
    position: u64,
    reads: &[AssetRead],
    factory: ProtocolDigest,
    max_bytes: usize,
) -> Result<RestoredContinuation<'a>, ServiceError> {
    // Select only a declared record id; Reader still validates the complete
    // framing, version, reserved fields, limits and checksum before any data.
    let is_tail =
        bytes.get(4..8) == Some(WAIT_TAIL_RESUME_STATE_SCHEMA.id.to_le_bytes().as_slice());
    let schema = if is_tail {
        WAIT_TAIL_RESUME_STATE_SCHEMA
    } else {
        RESUME_STATE_SCHEMA
    };
    let mut reader = Reader::open(
        bytes,
        schema,
        Limits {
            max_total: max_bytes,
            max_field: max_bytes,
        },
        UnknownPolicy::Strict,
    )
    .map_err(execution_error)?;
    if reader.get_str().map_err(execution_error)? != scene
        || reader.get_u64().map_err(execution_error)? != position
        || reader.get_u32().map_err(execution_error)? as usize != reads.len()
    {
        return Err(invalid(
            "native continuation scene, position or input closure differs",
        ));
    }
    for read in reads {
        if reader.get_str().map_err(execution_error)? != read.path
            || reader.get_bytes().map_err(execution_error)? != read.digest.as_bytes()
        {
            return Err(invalid("native continuation input closure differs"));
        }
    }
    if reader.get_bytes().map_err(execution_error)? != factory.as_bytes() {
        return Err(invalid("native continuation factory identity differs"));
    }
    let frame = reader.get_u64().map_err(execution_error)?;
    let frame_shape = [
        reader.get_u64().map_err(execution_error)?,
        reader.get_u64().map_err(execution_error)?,
    ];
    let started_segments =
        usize::try_from(reader.get_u64().map_err(execution_error)?).map_err(execution_error)?;
    let effects = reader.get_u8().map_err(execution_error)?;
    let tail = if is_tail {
        Some(NativeWaitTailCursor {
            index: usize::try_from(reader.get_u64().map_err(execution_error)?)
                .map_err(execution_error)?,
            prefix_frames: reader.get_i64().map_err(execution_error)?,
            prefix_segments: usize::try_from(reader.get_u64().map_err(execution_error)?)
                .map_err(execution_error)?,
            prefix_plays: reader.get_u64().map_err(execution_error)?,
        })
    } else {
        None
    };
    let active = if reader.get_bool().map_err(execution_error)? {
        Some(NativeWaitCursor {
            base_frame: reader.get_i64().map_err(execution_error)?,
            stepped: reader.get_i64().map_err(execution_error)?,
            purity: reader.get_u8().map_err(execution_error)?,
        })
    } else {
        None
    };
    let state = reader.get_bytes().map_err(execution_error)?;
    reader.finish().map_err(execution_error)?;
    Ok(RestoredContinuation {
        cursor: NativeProgramCheckpoint {
            frame,
            frame_shape,
            started_segments,
            effects,
            active,
            tail,
        },
        state,
    })
}
