//! Lossless captured snapshots in independently addressable 64-frame blocks.
//!
//! A patch always names bytes in its block's base, never another patch. The
//! canonical snapshot itself is opaque: validation may materialize a temporary
//! arena, but neither decoding nor encoding rewrites those captured bytes.

use std::borrow::Cow;
use std::collections::HashSet;
use std::mem::size_of;
use std::sync::Arc;

use fmn_hash::SerialError;
use fmn_hash::serial::{Reader, Writer};
use fmn_mobject::persist::PersistError;
use fmn_mobject::{BatchKey, IdBuildHasher, Mob, ProgramKind, Snapshot, Stage};

use super::{BundleError, BundleReadError};

pub(crate) const BLOCK_FRAMES: usize = 64;
pub(crate) const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_SNAPSHOT_WORK: u64 = 16 * 1024 * 1024 * 1024;
const MERGE_GAP: usize = 12;
const ARC_COUNTER_BYTES: usize = 2 * size_of::<usize>();

#[derive(Debug)]
struct Span {
    offset: u32,
    len: u32,
}

#[derive(Debug)]
struct Patch {
    target_len: u32,
    spans: Vec<Span>,
    bytes: Vec<u8>,
}

#[derive(Debug)]
enum Entry {
    Same,
    Literal(Vec<u8>),
    Patch(Patch),
}

/// A bounded portable unit of work. The fixed tail table is charged when its
/// base is admitted, so adding an identical frame cannot allocate a new table.
#[derive(Debug)]
pub(crate) struct CompactBlock {
    base: Vec<u8>,
    tail: [Entry; BLOCK_FRAMES - 1],
    frames: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CompactFrames {
    blocks: Vec<Arc<CompactBlock>>,
    frames: u32,
    retained: usize,
    camera: bool,
}

/// Incremental capture storage. Its blocks remain uniquely owned until
/// `finish`, which merely moves the already allocated storage into the result.
#[derive(Debug, Default)]
pub(crate) struct CompactFramesBuilder {
    frames: CompactFrames,
}

enum InputBytes<'a> {
    Owned(Vec<u8>),
    Borrowed(&'a [u8]),
}

impl InputBytes<'_> {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            Self::Borrowed(bytes) => bytes,
        }
    }

    fn retained_bytes(&self) -> usize {
        match self {
            Self::Owned(bytes) => bytes.capacity(),
            Self::Borrowed(bytes) => bytes.len(),
        }
    }

    fn into_owned(self) -> Result<Vec<u8>, BundleError> {
        match self {
            Self::Owned(bytes) => Ok(bytes),
            Self::Borrowed(bytes) => {
                let mut result = Vec::new();
                reserve_exact(&mut result, bytes.len(), "compact snapshot")?;
                result.extend_from_slice(bytes);
                Ok(result)
            }
        }
    }
}

impl CompactFramesBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn frame_count(&self) -> u32 {
        self.frames.frames
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.frames.retained
    }

    pub(crate) fn append(
        &mut self,
        snapshot: Vec<u8>,
        max_retained_bytes: usize,
    ) -> Result<(), BundleError> {
        check_snapshot_size(snapshot.len()).map_err(BundleError::Serial)?;
        let camera = validate_snapshot(&snapshot).map_err(BundleError::Persist)?;
        self.append_validated(InputBytes::Owned(snapshot), camera, max_retained_bytes)
    }

    pub(crate) fn finish(self) -> CompactFrames {
        self.frames
    }

    fn append_validated(
        &mut self,
        snapshot: InputBytes<'_>,
        camera: bool,
        limit: usize,
    ) -> Result<(), BundleError> {
        let next_count =
            self.frames
                .frames
                .checked_add(1)
                .ok_or(BundleError::FrameLimitExceeded {
                    frames: u64::from(u32::MAX) + 1,
                    max_frames: u64::from(u32::MAX),
                })?;
        if (self.frames.frames as usize).is_multiple_of(BLOCK_FRAMES) {
            let old_capacity = self.frames.blocks.capacity();
            let next_capacity = if self.frames.blocks.len() == old_capacity {
                old_capacity.saturating_mul(2).max(1)
            } else {
                old_capacity
            };
            let pointer_bytes = next_capacity
                .checked_sub(old_capacity)
                .and_then(|count| count.checked_mul(size_of::<Arc<CompactBlock>>()))
                .ok_or_else(|| capture_limit(usize::MAX, limit))?;
            let needed = self
                .frames
                .retained
                .checked_add(pointer_bytes)
                .and_then(|bytes| bytes.checked_add(size_of::<CompactBlock>()))
                .and_then(|bytes| bytes.checked_add(ARC_COUNTER_BYTES))
                .and_then(|bytes| bytes.checked_add(snapshot.retained_bytes()))
                .ok_or_else(|| capture_limit(usize::MAX, limit))?;
            require_retained(needed, limit)?;
            let block = Arc::new(CompactBlock {
                base: snapshot.into_owned()?,
                tail: std::array::from_fn(|_| Entry::Same),
                frames: 1,
            });
            if next_capacity > old_capacity {
                let additional = next_capacity - self.frames.blocks.len();
                reserve_exact(&mut self.frames.blocks, additional, "compact block table")?;
            }
            self.frames.blocks.push(block);
            self.frames.retained = needed;
        } else {
            let block = self
                .frames
                .blocks
                .last()
                .ok_or(BundleError::PlanDrift("compact base missing"))?;
            let plan = EncodingPlan::new(&block.base, snapshot.bytes());
            let extra = match plan {
                EncodingPlan::Same => 0,
                EncodingPlan::Literal => snapshot.retained_bytes(),
                EncodingPlan::Patch { spans, bytes } => spans
                    .checked_mul(size_of::<Span>())
                    .and_then(|metadata| metadata.checked_add(bytes))
                    .ok_or_else(|| capture_limit(usize::MAX, limit))?,
            };
            let needed = self
                .frames
                .retained
                .checked_add(extra)
                .ok_or_else(|| capture_limit(usize::MAX, limit))?;
            require_retained(needed, limit)?;
            let entry = match plan {
                EncodingPlan::Same => Entry::Same,
                EncodingPlan::Literal => Entry::Literal(snapshot.into_owned()?),
                EncodingPlan::Patch { spans, bytes } => {
                    Entry::Patch(make_patch(&block.base, snapshot.bytes(), spans, bytes)?)
                }
            };
            let block = self
                .frames
                .blocks
                .last_mut()
                .and_then(Arc::get_mut)
                .ok_or(BundleError::PlanDrift("compact capture block is shared"))?;
            block.tail[block.frames - 1] = entry;
            block.frames += 1;
            self.frames.retained = needed;
        }
        self.frames.frames = next_count;
        self.frames.camera |= camera;
        Ok(())
    }

    fn append_same(&mut self) -> Result<(), BundleReadError> {
        let block = self
            .frames
            .blocks
            .last_mut()
            .and_then(Arc::get_mut)
            .ok_or(BundleReadError::PlanInconsistent("compact base missing"))?;
        if block.frames >= BLOCK_FRAMES {
            return Err(BundleReadError::PlanInconsistent("compact block overflow"));
        }
        block.frames += 1;
        self.frames.frames =
            self.frames
                .frames
                .checked_add(1)
                .ok_or(BundleReadError::PlanInconsistent(
                    "compact frame count overflow",
                ))?;
        Ok(())
    }
}

impl CompactFrames {
    pub(crate) fn frame_count(&self) -> u32 {
        self.frames
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.retained
    }

    pub(crate) fn requires_camera(&self) -> bool {
        self.camera
    }

    pub(crate) fn frame(&self, index: usize) -> Option<(Arc<CompactBlock>, usize)> {
        if index >= self.frame_count() as usize {
            return None;
        }
        self.blocks
            .get(index / BLOCK_FRAMES)
            .map(|block| (Arc::clone(block), index % BLOCK_FRAMES))
    }

    pub(crate) fn materialize(&self, index: usize) -> Result<Stage, BundleReadError> {
        let (block, local) = self.frame(index).ok_or(BundleReadError::FrameOutOfRange {
            index: u32::try_from(index).unwrap_or(u32::MAX),
            total: self.frames,
        })?;
        block.materialize(local)
    }

    pub(crate) fn write_to(&self, writer: &mut Writer) -> Result<(), BundleError> {
        writer.put_u32(self.frame_count());
        for block in &self.blocks {
            writer.put_bytes(&block.base);
            for entry in block.tail.iter().take(block.frame_count() - 1) {
                match entry {
                    Entry::Same => {
                        writer.put_u8(0);
                    }
                    Entry::Literal(bytes) => {
                        writer.put_u8(1).put_bytes(bytes);
                    }
                    Entry::Patch(patch) => {
                        writer.put_u8(2).put_u32(patch.target_len).put_u32(
                            u32::try_from(patch.spans.len()).map_err(|_| {
                                BundleError::PlanDrift("compact patch span count overflow")
                            })?,
                        );
                        let mut start = 0;
                        for span in &patch.spans {
                            let end = start + span.len as usize;
                            writer
                                .put_u32(span.offset)
                                .put_bytes(&patch.bytes[start..end]);
                            start = end;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn read_from(
        reader: &mut Reader<'_>,
        expected_frames: u32,
        remaining_snapshot_work: &mut u64,
        remaining_retained_bytes: &mut usize,
    ) -> Result<Self, BundleReadError> {
        let count = reader.get_u32().map_err(BundleReadError::Malformed)?;
        if count != expected_frames {
            return Err(BundleReadError::PlanInconsistent("compact frame count"));
        }
        let blocks = (count as usize).div_ceil(BLOCK_FRAMES);
        let minimum = blocks
            .checked_mul(8)
            .and_then(|bytes| bytes.checked_add(count as usize - blocks))
            .ok_or(BundleReadError::PlanInconsistent(
                "compact frame table overflow",
            ))?;
        if minimum > reader.remaining() {
            return Err(BundleReadError::PlanInconsistent(
                "truncated compact frame table",
            ));
        }
        let mut builder = CompactFramesBuilder::new();
        for block_index in 0..blocks {
            let base = reader.get_bytes().map_err(BundleReadError::Malformed)?;
            check_snapshot_size(base.len()).map_err(BundleReadError::Malformed)?;
            charge_work(base.len(), remaining_snapshot_work)?;
            let camera = validate_snapshot(base).map_err(BundleReadError::Snapshot)?;
            append_read(
                &mut builder,
                InputBytes::Borrowed(base),
                camera,
                remaining_retained_bytes,
            )?;
            let in_block = (count as usize - block_index * BLOCK_FRAMES).min(BLOCK_FRAMES);
            for _ in 1..in_block {
                let tag = reader.get_u8().map_err(BundleReadError::Malformed)?;
                match tag {
                    0 => {
                        let base_len = builder.frames.blocks[block_index].base.len();
                        charge_work(base_len, remaining_snapshot_work)?;
                        builder.append_same()?;
                    }
                    1 => {
                        let bytes = reader.get_bytes().map_err(BundleReadError::Malformed)?;
                        check_snapshot_size(bytes.len()).map_err(BundleReadError::Malformed)?;
                        charge_work(bytes.len(), remaining_snapshot_work)?;
                        let camera = validate_snapshot(bytes).map_err(BundleReadError::Snapshot)?;
                        append_read(
                            &mut builder,
                            InputBytes::Borrowed(bytes),
                            camera,
                            remaining_retained_bytes,
                        )?;
                    }
                    2 => {
                        let base = &builder.frames.blocks[block_index].base;
                        let bytes = read_patch(reader, base, remaining_snapshot_work)?;
                        let camera =
                            validate_snapshot(&bytes).map_err(BundleReadError::Snapshot)?;
                        append_read(
                            &mut builder,
                            InputBytes::Owned(bytes),
                            camera,
                            remaining_retained_bytes,
                        )?;
                    }
                    _ => {
                        return Err(BundleReadError::PlanInconsistent(
                            "unknown compact frame tag",
                        ));
                    }
                }
            }
        }
        Ok(builder.finish())
    }
}

impl CompactBlock {
    pub(crate) fn frame_count(&self) -> usize {
        self.frames
    }

    /// Identity for decoded-snapshot reuse within this exact block. It does not
    /// claim semantic equality between distinct literal or patch entries.
    pub(crate) fn snapshot_index(&self, index: usize) -> Option<usize> {
        if index >= self.frames {
            None
        } else if index == 0 || matches!(self.tail[index - 1], Entry::Same) {
            Some(0)
        } else {
            Some(index)
        }
    }

    pub(crate) fn snapshot_bytes(&self, index: usize) -> Result<Cow<'_, [u8]>, BundleReadError> {
        if index >= self.frames {
            return Err(BundleReadError::FrameOutOfRange {
                index: u32::try_from(index).unwrap_or(u32::MAX),
                total: self.frames as u32,
            });
        }
        if index == 0 {
            return Ok(Cow::Borrowed(&self.base));
        }
        match &self.tail[index - 1] {
            Entry::Same => Ok(Cow::Borrowed(&self.base)),
            Entry::Literal(bytes) => Ok(Cow::Borrowed(bytes)),
            Entry::Patch(patch) => {
                let mut bytes = snapshot_buffer(&self.base, patch.target_len as usize)?;
                let mut start = 0;
                for span in &patch.spans {
                    let end = start + span.len as usize;
                    let offset = span.offset as usize;
                    bytes[offset..offset + span.len as usize]
                        .copy_from_slice(&patch.bytes[start..end]);
                    start = end;
                }
                Ok(Cow::Owned(bytes))
            }
        }
    }

    pub(crate) fn materialize(&self, index: usize) -> Result<Stage, BundleReadError> {
        let bytes = self.snapshot_bytes(index)?;
        Snapshot::from_bytes(&bytes, &Stage::new())
            .map(|decoded| decoded.snapshot.materialize())
            .map_err(BundleReadError::Snapshot)
    }
}

#[derive(Clone, Copy)]
enum EncodingPlan {
    Same,
    Literal,
    Patch { spans: usize, bytes: usize },
}

impl EncodingPlan {
    fn new(base: &[u8], target: &[u8]) -> Self {
        if base == target {
            return Self::Same;
        }
        let mut spans = 0usize;
        let mut bytes = 0usize;
        for (start, end) in ChangedSpans::new(base, target) {
            spans += 1;
            bytes += end - start;
        }
        // Snapshot lengths are bounded to 64 MiB before this calculation.
        let patch_wire = 9 + spans * 12 + bytes;
        let literal_wire = 9 + target.len();
        if patch_wire < literal_wire {
            Self::Patch { spans, bytes }
        } else {
            Self::Literal
        }
    }
}

struct ChangedSpans<'a> {
    base: &'a [u8],
    target: &'a [u8],
    next: usize,
}

impl<'a> ChangedSpans<'a> {
    fn new(base: &'a [u8], target: &'a [u8]) -> Self {
        Self {
            base,
            target,
            next: 0,
        }
    }

    fn changed(&self, index: usize) -> bool {
        self.base.get(index) != self.target.get(index)
    }
}

impl Iterator for ChangedSpans<'_> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<Self::Item> {
        while self.next < self.target.len() && !self.changed(self.next) {
            self.next += 1;
        }
        if self.next == self.target.len() {
            return None;
        }
        let start = self.next;
        let mut end = start + 1;
        self.next += 1;
        while self.next < self.target.len() {
            if self.changed(self.next) {
                end = self.next + 1;
            } else if self.next - end >= MERGE_GAP {
                self.next += 1;
                break;
            }
            self.next += 1;
        }
        Some((start, end))
    }
}

fn make_patch(
    base: &[u8],
    target: &[u8],
    span_count: usize,
    byte_count: usize,
) -> Result<Patch, BundleError> {
    let mut spans = Vec::new();
    let mut bytes = Vec::new();
    reserve_exact(&mut spans, span_count, "compact patch spans")?;
    reserve_exact(&mut bytes, byte_count, "compact patch bytes")?;
    for (start, end) in ChangedSpans::new(base, target) {
        spans.push(Span {
            offset: start as u32,
            len: (end - start) as u32,
        });
        bytes.extend_from_slice(&target[start..end]);
    }
    Ok(Patch {
        target_len: target.len() as u32,
        spans,
        bytes,
    })
}

fn read_patch(
    reader: &mut Reader<'_>,
    base: &[u8],
    remaining_work: &mut u64,
) -> Result<Vec<u8>, BundleReadError> {
    let target_len = reader.get_u32().map_err(BundleReadError::Malformed)? as usize;
    check_snapshot_size(target_len).map_err(BundleReadError::Malformed)?;
    let count = reader.get_u32().map_err(BundleReadError::Malformed)? as usize;
    let extension = target_len.saturating_sub(base.len());
    let minimum = count
        .checked_mul(12)
        .and_then(|bytes| bytes.checked_add(count.max(extension)))
        .ok_or(BundleReadError::PlanInconsistent(
            "compact patch span count overflow",
        ))?;
    if count > target_len || minimum > reader.remaining() || (extension != 0 && count == 0) {
        return Err(BundleReadError::PlanInconsistent(
            "truncated compact patch spans",
        ));
    }
    charge_work(target_len, remaining_work)?;
    let mut target = snapshot_buffer(base, target_len)?;
    let mut previous_end = 0;
    let mut covered_extension = base.len().min(target_len);
    for _ in 0..count {
        let offset = reader.get_u32().map_err(BundleReadError::Malformed)? as usize;
        let bytes = reader.get_bytes().map_err(BundleReadError::Malformed)?;
        let end = offset
            .checked_add(bytes.len())
            .ok_or(BundleReadError::PlanInconsistent(
                "compact patch offset overflow",
            ))?;
        if bytes.is_empty() || offset < previous_end || end > target_len {
            return Err(BundleReadError::PlanInconsistent(
                "invalid compact patch span",
            ));
        }
        if end > base.len() {
            if offset > covered_extension {
                return Err(BundleReadError::PlanInconsistent(
                    "compact patch extension hole",
                ));
            }
            covered_extension = end;
        }
        target[offset..end].copy_from_slice(bytes);
        previous_end = end;
    }
    if covered_extension != target_len && target_len > base.len() {
        return Err(BundleReadError::PlanInconsistent(
            "compact patch extension is incomplete",
        ));
    }
    Ok(target)
}

fn snapshot_buffer(base: &[u8], target_len: usize) -> Result<Vec<u8>, BundleReadError> {
    check_snapshot_size(target_len).map_err(BundleReadError::Malformed)?;
    let mut target = Vec::new();
    target
        .try_reserve_exact(target_len)
        .map_err(|_| BundleReadError::AllocationFailed {
            context: "compact reconstructed snapshot",
            requested: target_len,
        })?;
    target.extend_from_slice(&base[..base.len().min(target_len)]);
    target.resize(target_len, 0);
    Ok(target)
}

fn validate_snapshot(bytes: &[u8]) -> Result<bool, PersistError> {
    let decoded = Snapshot::from_bytes(bytes, &Stage::new())?;
    let (required, _) = reachable_camera_requirement(&decoded.snapshot.materialize())?;
    Ok(required)
}

/// An existential camera query must not expand the repeated draw sequence.
/// Valid snapshots can place one large shared family under many scene roots.
/// Each live vertex is enqueued at most once, independent of placement count.
/// The admitted vertex count also gives the regression a deterministic work
/// bound without allocating the potentially enormous draw plan it replaces.
fn reachable_camera_requirement(stage: &Stage) -> Result<(bool, usize), PersistError> {
    let mut seen = HashSet::<Mob, IdBuildHasher>::default();
    let mut pending = Vec::new();
    for &root in stage.roots() {
        enqueue_reachable(root, &mut seen, &mut pending)?;
    }
    while let Some(mob) = pending.pop() {
        let Some(entry) = stage.get(mob) else {
            continue;
        };
        if !entry.buffer.is_empty()
            && entry.paints()
            && BatchKey::of(stage, mob)
                .is_some_and(|key| key.program != ProgramKind::Vector || key.uniforms.depth_test)
        {
            return Ok((true, seen.len()));
        }
        for &child in entry.submobjects() {
            enqueue_reachable(child, &mut seen, &mut pending)?;
        }
    }
    Ok((false, seen.len()))
}

fn enqueue_reachable(
    mob: Mob,
    seen: &mut HashSet<Mob, IdBuildHasher>,
    pending: &mut Vec<Mob>,
) -> Result<(), PersistError> {
    if seen.contains(&mob) {
        return Ok(());
    }
    seen.try_reserve(1).map_err(|_| {
        PersistError::Serial(SerialError::AllocationFailed {
            needed: seen
                .len()
                .saturating_add(1)
                .saturating_mul(size_of::<Mob>()),
        })
    })?;
    pending.try_reserve(1).map_err(|_| {
        PersistError::Serial(SerialError::AllocationFailed {
            needed: pending
                .len()
                .saturating_add(1)
                .saturating_mul(size_of::<Mob>()),
        })
    })?;
    seen.insert(mob);
    pending.push(mob);
    Ok(())
}

fn check_snapshot_size(len: usize) -> Result<(), SerialError> {
    if len > MAX_SNAPSHOT_BYTES {
        Err(SerialError::SizeLimit {
            limit: MAX_SNAPSHOT_BYTES,
            needed: len,
        })
    } else {
        Ok(())
    }
}

fn charge_work(len: usize, remaining: &mut u64) -> Result<(), BundleReadError> {
    *remaining = remaining
        .checked_sub(len as u64)
        .ok_or(BundleReadError::PlanInconsistent(
            "compact snapshot work budget exceeded",
        ))?;
    Ok(())
}

fn append_read(
    builder: &mut CompactFramesBuilder,
    bytes: InputBytes<'_>,
    camera: bool,
    remaining_retained: &mut usize,
) -> Result<(), BundleReadError> {
    let before = builder.retained_bytes();
    let ceiling =
        before
            .checked_add(*remaining_retained)
            .ok_or(BundleReadError::PlanInconsistent(
                "compact retained budget overflow",
            ))?;
    builder
        .append_validated(bytes, camera, ceiling)
        .map_err(read_error)?;
    *remaining_retained -= builder.retained_bytes() - before;
    Ok(())
}

fn read_error(error: BundleError) -> BundleReadError {
    match error {
        BundleError::Serial(error) => BundleReadError::Malformed(error),
        BundleError::Persist(error) => BundleReadError::Snapshot(error),
        BundleError::PlanDrift(reason) => BundleReadError::PlanInconsistent(reason),
        BundleError::CaptureLimitExceeded { needed, limit, .. } => {
            BundleReadError::Malformed(SerialError::SizeLimit { limit, needed })
        }
        BundleError::AllocationFailed { context, requested } => {
            BundleReadError::AllocationFailed { context, requested }
        }
        BundleError::FrameLimitExceeded { .. } => {
            BundleReadError::PlanInconsistent("compact frame count overflow")
        }
        BundleError::Anim(_) => {
            BundleReadError::PlanInconsistent("compact capture animation error")
        }
    }
}

fn capture_limit(needed: usize, limit: usize) -> BundleError {
    BundleError::CaptureLimitExceeded {
        context: "compact frame storage",
        needed,
        limit,
    }
}

fn require_retained(needed: usize, limit: usize) -> Result<(), BundleError> {
    if needed > limit {
        Err(capture_limit(needed, limit))
    } else {
        Ok(())
    }
}

fn reserve_exact<T>(
    vec: &mut Vec<T>,
    additional: usize,
    context: &'static str,
) -> Result<(), BundleError> {
    vec.try_reserve_exact(additional)
        .map_err(|_| BundleError::AllocationFailed {
            context,
            requested: additional,
        })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use fmn_hash::serial::{Limits, Schema, UnknownPolicy};
    use fmn_mobject::Mobject;

    use super::*;

    const TEST_SCHEMA: Schema = Schema::new(*b"FMCT", 1, 0, 0);

    fn reader(bytes: &[u8]) -> Reader<'_> {
        Reader::open(bytes, TEST_SCHEMA, Limits::DEFAULT, UnknownPolicy::Strict).unwrap()
    }

    fn encode(frames: &CompactFrames) -> Vec<u8> {
        let mut writer = Writer::new(TEST_SCHEMA);
        frames.write_to(&mut writer).unwrap();
        writer.finish().unwrap()
    }

    fn decode(bytes: &[u8], count: u32) -> CompactFrames {
        let mut input = reader(bytes);
        let mut work = MAX_SNAPSHOT_WORK;
        let mut retained = Limits::DEFAULT.max_total;
        let frames = CompactFrames::read_from(&mut input, count, &mut work, &mut retained).unwrap();
        input.finish().unwrap();
        frames
    }

    fn stage_with_point() -> (Stage, fmn_mobject::Mob) {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_points(&[[0.0, 0.0, 0.0]]));
        stage.add_to_scene(mob).unwrap();
        (stage, mob)
    }

    fn snapshot_at(x: f64) -> Vec<u8> {
        let (mut stage, mob) = stage_with_point();
        stage.set_x(mob, x);
        stage.snapshot_bytes().unwrap()
    }

    #[test]
    fn compact_exact_bytes_cross_blocks_and_reverse_random_access() {
        let (mut stage, mob) = stage_with_point();
        let mut builder = CompactFramesBuilder::new();
        let mut snapshots = Vec::new();
        for index in 0..131 {
            stage.set_x(mob, f64::from(index) / 16.0);
            // Crossing both block boundaries with actual arena topology
            // changes also exercises snapshot length growth.
            if index == 20 || index == 100 {
                let extra = stage.add(Mobject::from_points(&[[1.0, 2.0, 3.0]; 19]));
                stage.add_to_scene(extra).unwrap();
            }
            let bytes = stage.snapshot_bytes().unwrap();
            builder
                .append(bytes.clone(), Limits::DEFAULT.max_total)
                .unwrap();
            snapshots.push(bytes);
        }
        let retained = builder.retained_bytes();
        let frames = builder.finish();
        assert_eq!(
            frames.retained_bytes(),
            retained,
            "finish adds no allocation"
        );
        assert_eq!(frames.frame_count(), 131);
        let encoded = encode(&frames);
        let decoded = decode(&encoded, 131);
        assert_eq!(
            encoded,
            encode(&decoded),
            "canonical encoding is deterministic"
        );
        assert!(decoded.retained_bytes() < snapshots.iter().map(Vec::len).sum::<usize>());
        for index in (0..snapshots.len()).rev().chain([0, 64, 128, 63, 129]) {
            let (block, local) = decoded.frame(index).unwrap();
            assert_eq!(
                block.snapshot_bytes(local).unwrap().as_ref(),
                snapshots[index]
            );
            assert_eq!(
                decoded
                    .materialize(index)
                    .unwrap()
                    .snapshot_bytes()
                    .unwrap(),
                snapshots[index]
            );
        }
        assert!(decoded.frame(131).is_none());
        assert!(decoded.materialize(usize::MAX).is_err());
    }

    #[test]
    fn compact_same_frames_share_only_their_own_block_base() {
        let snapshot = snapshot_at(1.0);
        let mut builder = CompactFramesBuilder::new();
        for _ in 0..65 {
            builder
                .append(snapshot.clone(), Limits::DEFAULT.max_total)
                .unwrap();
        }
        let frames = decode(&encode(&builder.finish()), 65);
        let (first, _) = frames.frame(0).unwrap();
        let (last_in_block, local) = frames.frame(63).unwrap();
        let (second, _) = frames.frame(64).unwrap();
        assert!(Arc::ptr_eq(&first, &last_in_block));
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(first.frame_count(), 64);
        assert_eq!(second.frame_count(), 1);
        assert_eq!(last_in_block.snapshot_index(local), Some(0));
        assert_eq!(first.snapshot_index(64), None);
        assert_eq!(second.snapshot_index(1), None);
        assert!(matches!(
            first.snapshot_bytes(63).unwrap(),
            Cow::Borrowed(_)
        ));
        assert!(encode(&frames).len() < snapshot.len() * 3);
    }

    #[test]
    fn compact_changed_spans_merge_twelve_byte_gaps_and_literal_wins_ties() {
        let base = vec![0; 128];
        let mut target = base.clone();
        target[10] = 1;
        target[23] = 2; // Exactly twelve equal bytes: merge.
        target[37] = 3; // Thirteen equal bytes: split.
        assert_eq!(
            ChangedSpans::new(&base, &target).collect::<Vec<_>>(),
            vec![(10, 24), (37, 38)]
        );
        assert!(matches!(
            EncodingPlan::new(&base, &target),
            EncodingPlan::Patch {
                spans: 2,
                bytes: 15
            }
        ));
        let mut tie = vec![0; 13];
        tie[0] = 1;
        assert!(matches!(
            EncodingPlan::new(&[0; 13], &tie),
            EncodingPlan::Literal
        ));
        assert!(matches!(
            EncodingPlan::new(&base, &base),
            EncodingPlan::Same
        ));
        assert!(matches!(
            EncodingPlan::new(&base, &[]),
            EncodingPlan::Literal
        ));
    }

    #[test]
    fn compact_delta_round_trip_covers_length_changes_and_zero_extensions() {
        // Fixed deterministic bytes exercise the codec independently of the
        // snapshot parser, including empty/truncated inputs and long gaps.
        for base_len in [0, 1, 12, 13, 64, 257, 1024] {
            let base: Vec<u8> = (0..base_len)
                .map(|index| (index * 71 % 251) as u8)
                .collect();
            for target_len in [0, 1, 12, 64, 100, 257, 1500] {
                let mut target = base[..base_len.min(target_len)].to_vec();
                target.resize(target_len, 0);
                for index in (3..target_len).step_by(31) {
                    target[index] ^= 0x5a;
                }
                let entry = match EncodingPlan::new(&base, &target) {
                    EncodingPlan::Same => Entry::Same,
                    EncodingPlan::Literal => Entry::Literal(target.clone()),
                    EncodingPlan::Patch { spans, bytes } => {
                        Entry::Patch(make_patch(&base, &target, spans, bytes).unwrap())
                    }
                };
                let mut block = CompactBlock {
                    base: base.clone(),
                    tail: std::array::from_fn(|_| Entry::Same),
                    frames: 2,
                };
                block.tail[0] = entry;
                assert_eq!(
                    block.snapshot_bytes(1).unwrap().as_ref(),
                    target,
                    "lengths {base_len} -> {target_len}"
                );
                if target_len > base_len {
                    let spans: Vec<_> = ChangedSpans::new(&base, &target).collect();
                    assert_eq!(
                        spans.last().unwrap().1,
                        target_len,
                        "zero extension is explicit literal data"
                    );
                }
            }
        }
    }

    fn patch_document(target_len: u32, spans: &[(u32, &[u8])]) -> Vec<u8> {
        let mut writer = Writer::new(TEST_SCHEMA);
        writer.put_u32(target_len).put_u32(spans.len() as u32);
        for (offset, bytes) in spans {
            writer.put_u32(*offset).put_bytes(bytes);
        }
        writer.finish().unwrap()
    }

    fn decode_patch(bytes: &[u8], base: &[u8]) -> Result<Vec<u8>, BundleReadError> {
        let mut work = MAX_SNAPSHOT_WORK;
        read_patch(&mut reader(bytes), base, &mut work)
    }

    #[test]
    fn compact_patch_rejects_overlap_order_empty_bounds_and_extension_holes() {
        let base = [7; 32];
        type SpanCase<'a> = (u32, &'a [(u32, &'a [u8])]);
        let invalid: &[SpanCase<'_>] = &[
            (32, &[(1, &[2, 3]), (2, &[4])]),
            (32, &[(20, &[2]), (4, &[3])]),
            (32, &[(1, &[])]),
            (32, &[(32, &[1])]),
            (32, &[(u32::MAX, &[1])]),
            (35, &[(33, &[0, 0])]),
            (35, &[(32, &[0, 0])]),
            (35, &[(31, &[0, 0]), (34, &[0])]),
            (35, &[]),
        ];
        for (length, spans) in invalid {
            let bytes = patch_document(*length, spans);
            assert!(
                decode_patch(&bytes, &base).is_err(),
                "accepted {length} {spans:?}"
            );
        }
        let bytes = patch_document(35, &[(31, &[0, 0, 0, 0])]);
        let actual = decode_patch(&bytes, &base).unwrap();
        assert_eq!(&actual[..31], &base[..31]);
        assert_eq!(&actual[31..], &[0; 4]);
        let truncate = patch_document(7, &[]);
        assert_eq!(decode_patch(&truncate, &base).unwrap(), base[..7]);
    }

    #[test]
    fn compact_patch_refuses_length_and_count_bombs_before_reconstruction() {
        for (length, count) in [
            (MAX_SNAPSHOT_BYTES as u32 + 1, 0),
            (32, u32::MAX),
            (MAX_SNAPSHOT_BYTES as u32, 1),
        ] {
            let mut writer = Writer::new(TEST_SCHEMA);
            writer.put_u32(length).put_u32(count);
            let bytes = writer.finish().unwrap();
            let mut work = MAX_SNAPSHOT_WORK;
            assert!(read_patch(&mut reader(&bytes), &[1; 32], &mut work).is_err());
            assert_eq!(
                work, MAX_SNAPSHOT_WORK,
                "structural preflight precedes allocation and work"
            );
        }
        let mut writer = Writer::new(TEST_SCHEMA);
        writer.put_u32(u32::MAX);
        let bytes = writer.finish().unwrap();
        let mut retained = Limits::DEFAULT.max_total;
        let mut work = MAX_SNAPSHOT_WORK;
        assert!(
            CompactFrames::read_from(&mut reader(&bytes), u32::MAX, &mut work, &mut retained)
                .is_err()
        );
        assert_eq!(retained, Limits::DEFAULT.max_total);
        assert_eq!(work, MAX_SNAPSHOT_WORK);
    }

    #[test]
    fn compact_capture_refusal_is_atomic_and_finish_has_no_extra_charge() {
        let base = snapshot_at(0.0);
        let changed = snapshot_at(2.0);
        let mut builder = CompactFramesBuilder::new();
        assert!(matches!(
            builder.append(base.clone(), 0),
            Err(BundleError::CaptureLimitExceeded { .. })
        ));
        assert_eq!(builder.frame_count(), 0);
        assert_eq!(builder.retained_bytes(), 0);
        builder
            .append(base.clone(), Limits::DEFAULT.max_total)
            .unwrap();
        let base_charge = builder.retained_bytes();
        assert!(matches!(
            builder.append(changed.clone(), base_charge),
            Err(BundleError::CaptureLimitExceeded { .. })
        ));
        assert_eq!(builder.frame_count(), 1);
        assert_eq!(builder.retained_bytes(), base_charge);
        for _ in 1..64 {
            builder.append(base.clone(), base_charge).unwrap();
        }
        assert_eq!(builder.retained_bytes(), base_charge);
        assert!(
            builder.append(base, base_charge).is_err(),
            "a new block needs its own base and metadata"
        );
        assert_eq!(builder.frame_count(), 64);
        let frames = builder.finish();
        assert_eq!(frames.retained_bytes(), base_charge);
        assert_eq!(frames.frame_count(), 64);
    }

    #[test]
    fn compact_reader_charges_same_frames_and_shares_budgets_across_segments() {
        let snapshot = snapshot_at(0.0);
        let mut builder = CompactFramesBuilder::new();
        for _ in 0..4 {
            builder
                .append(snapshot.clone(), Limits::DEFAULT.max_total)
                .unwrap();
        }
        let frames = builder.finish();
        let storage = frames.retained_bytes();
        let mut writer = Writer::new(TEST_SCHEMA);
        frames.write_to(&mut writer).unwrap();
        frames.write_to(&mut writer).unwrap();
        let bytes = writer.finish().unwrap();
        let per_segment_work = snapshot.len() as u64 * 4;
        let mut work = per_segment_work * 2;
        let mut retained = storage;
        let mut input = reader(&bytes);
        CompactFrames::read_from(&mut input, 4, &mut work, &mut retained).unwrap();
        assert_eq!(work, per_segment_work);
        assert_eq!(retained, 0);
        assert!(matches!(
            CompactFrames::read_from(&mut input, 4, &mut work, &mut retained),
            Err(BundleReadError::Malformed(SerialError::SizeLimit { .. }))
        ));
        assert_eq!(
            retained, 0,
            "the second segment cannot consume unavailable encoded storage"
        );

        let single = encode(&frames);
        let mut work = per_segment_work - 1;
        let mut retained = Limits::DEFAULT.max_total;
        assert!(matches!(
            CompactFrames::read_from(&mut reader(&single), 4, &mut work, &mut retained),
            Err(BundleReadError::PlanInconsistent(
                "compact snapshot work budget exceeded"
            ))
        ));
    }

    #[test]
    fn compact_reader_rejects_bad_counts_tags_truncated_spans_and_snapshot_checksum() {
        let base = snapshot_at(0.0);
        let refusal = |bytes: &[u8], expected| {
            let mut work = MAX_SNAPSHOT_WORK;
            let mut retained = Limits::DEFAULT.max_total;
            CompactFrames::read_from(&mut reader(bytes), expected, &mut work, &mut retained)
                .is_err()
        };
        let mut writer = Writer::new(TEST_SCHEMA);
        writer.put_u32(1).put_bytes(&base);
        assert!(refusal(&writer.finish().unwrap(), 2));

        let mut writer = Writer::new(TEST_SCHEMA);
        writer.put_u32(2).put_bytes(&base).put_u8(3);
        assert!(refusal(&writer.finish().unwrap(), 2));

        let mut writer = Writer::new(TEST_SCHEMA);
        writer
            .put_u32(2)
            .put_bytes(&base)
            .put_u8(2)
            .put_u32(base.len() as u32)
            .put_u32(1)
            .put_u32(0)
            .put_u64(99);
        assert!(refusal(&writer.finish().unwrap(), 2));

        // The outer document has a valid recomputed checksum: the nested
        // snapshot checksum, not container framing, must reject this data.
        let mut corrupt = base.clone();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 1;
        let mut writer = Writer::new(TEST_SCHEMA);
        writer.put_u32(1).put_bytes(&corrupt);
        assert!(refusal(&writer.finish().unwrap(), 1));
    }

    #[test]
    fn compact_camera_query_visits_shared_vertices_once_and_ignores_detached_or_drawless_entries() {
        let mut stage = Stage::new();
        let root = stage.add(Mobject::group(
            (0..256)
                .map(|_| Mobject::vector_from_points(&[[0.0, 0.0, 0.0]]))
                .collect(),
        ));
        let bare = stage.add(Mobject::from_points(&[[0.0, 0.0, 0.0]]));
        let empty = stage.add(Mobject::vector_from_points(&[]));
        let detached = stage.add(Mobject::vector_from_points(&[[0.0, 0.0, 0.0]]));
        for mob in [bare, empty, detached] {
            stage.get_mut(mob).unwrap().uniforms_mut().depth_test = true;
        }
        stage.attach(root, bare).unwrap();
        stage.attach(root, empty).unwrap();
        stage.add_many_to_scene(&[root; 128]).unwrap();
        assert_eq!(
            stage.roots().len(),
            128,
            "duplicate placements remain legal"
        );
        let (required, admitted_vertices) = reachable_camera_requirement(&stage).unwrap();
        assert!(!required);
        assert_eq!(
            admitted_vertices, 259,
            "one group, 256 vectors, two drawless children; detached entry excluded"
        );

        let bytes = stage.snapshot_bytes().unwrap();
        let mut builder = CompactFramesBuilder::new();
        builder.append(bytes, Limits::DEFAULT.max_total).unwrap();
        let frames = decode(&encode(&builder.finish()), 1);
        assert!(!frames.requires_camera());

        stage.attach(root, detached).unwrap();
        assert!(reachable_camera_requirement(&stage).unwrap().0);
    }

    #[test]
    fn compact_preserves_updater_manifest_bytes_and_camera_requirement_without_callbacks() {
        let mut stage = Stage::new();
        let mut line =
            Mobject::vector_from_points(&[[-1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [1.0, 0.0, 0.0]]);
        line.buffer
            .write_color_lanes("stroke_rgba", Some([1.0, 1.0, 1.0]), Some(1.0));
        line.buffer.write_scalar_lanes("stroke_width", 2.0);
        line.uniforms.depth_test = true;
        let mob = stage.add(line);
        stage.add_to_scene(mob).unwrap();
        assert!(!stage.draw_plan().items().is_empty());
        let calls = Rc::new(Cell::new(0));
        let counter = Rc::clone(&calls);
        stage
            .add_updater(mob, move |_, _| counter.set(counter.get() + 1), false)
            .unwrap();
        let bytes = stage.snapshot_bytes().unwrap();
        let mut builder = CompactFramesBuilder::new();
        builder
            .append(bytes.clone(), Limits::DEFAULT.max_total)
            .unwrap();
        builder
            .append(bytes.clone(), Limits::DEFAULT.max_total)
            .unwrap();
        let frames = decode(&encode(&builder.finish()), 2);
        assert!(frames.requires_camera());
        let (block, local) = frames.frame(1).unwrap();
        let captured = block.snapshot_bytes(local).unwrap();
        assert_eq!(captured.as_ref(), bytes);
        let decoded = Snapshot::from_bytes(&captured, &Stage::new()).unwrap();
        assert_eq!(decoded.updaters.entries.len(), 1);
        assert_eq!(decoded.updaters.entries[0].1.len(), 1);
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn compact_portable_blocks_outlive_the_reader_and_materialize_on_workers() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CompactFrames>();
        assert_send_sync::<CompactBlock>();
        let mut builder = CompactFramesBuilder::new();
        for index in 0..8 {
            builder
                .append(snapshot_at(f64::from(index)), Limits::DEFAULT.max_total)
                .unwrap();
        }
        let frames = decode(&encode(&builder.finish()), 8);
        let jobs: Vec<_> = (0..8)
            .rev()
            .map(|index| (index, frames.frame(index).unwrap()))
            .collect();
        drop(frames);
        let workers: Vec<_> = jobs
            .into_iter()
            .map(|(index, (block, local))| {
                std::thread::spawn(move || {
                    let stage = block.materialize(local).unwrap();
                    assert_eq!(stage.get_center(stage.roots()[0])[0], index as f64);
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[test]
    fn compact_static_geometry_with_changed_tracker_retains_a_fraction_of_capture_bytes() {
        let mut stage = Stage::new();
        let points: Vec<_> = (0..4096)
            .map(|index| [f64::from(index) / 64.0, 0.0, 0.0])
            .collect();
        let mesh = stage.add(Mobject::from_points(&points));
        let tracker = stage.add_value_tracker(0.0);
        stage.add_many_to_scene(&[mesh, tracker]).unwrap();
        let mut builder = CompactFramesBuilder::new();
        let mut canonical_work = 0;
        let mut last = Vec::new();
        for index in 0..64 {
            stage.set_tracker_value(tracker, f64::from(index)).unwrap();
            let bytes = stage.snapshot_bytes().unwrap();
            canonical_work += bytes.len();
            last = bytes.clone();
            builder.append(bytes, Limits::DEFAULT.max_total).unwrap();
        }
        assert!(builder.retained_bytes() * 30 < canonical_work);
        let frames = builder.finish();
        let (block, local) = frames.frame(63).unwrap();
        assert_eq!(block.snapshot_bytes(local).unwrap().as_ref(), last);
        assert_eq!(
            block.snapshot_index(local),
            Some(63),
            "changed captures retain their own cache identity"
        );
    }
}
