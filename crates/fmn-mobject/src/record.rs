//! The RecordBuffer: Marionette's data plane (§8.2, §1.1, D-20) — the
//! authoritative interleaved f32 buffer, the G0-1-ratified view protocol,
//! and the lazy revisioned render mirrors.
//!
//! **Layer 1 — the authoritative buffer.** Interleaved f32 records matching
//! the Reference dtypes exactly ([`RecordSchema::mobject`],
//! [`RecordSchema::vmobject`]); user-declared custom dtypes ride the same
//! schema machinery. The interleaved layout is API surface: `mobject.data`
//! exports as a zero-copy NumPy structured array (byte offsets locked by
//! test against numpy's packing). `aligned_data_keys` /
//! `pointlike_data_keys`, data locking as copy-elision markers,
//! resize-with-interpolation, and null-padding all live here.
//!
//! **Layer 2 — the view protocol** (V1–V6 of
//! `docs/g0/G0-1-object-model-ratification.md`):
//! - V1 storage generations are fixed-capacity `Arc`-owned allocations;
//!   growth swaps in a fresh generation (reallocation under a live view is
//!   impossible by construction);
//! - V2 views pin their generation;
//! - V3 live views alias the current generation, resize/restore detaches
//!   them with NumPy-natural semantics;
//! - V4 every write bumps revision counters — render state is dirty by
//!   comparison, never by eager notification;
//! - V5 a generation is never simultaneously snapshot-shared and
//!   view-aliased (writers/view-exporters unshare; snapshots eagerly copy
//!   viewed buffers);
//! - V6 restore behaves as resize.
//!
//! Views may be whole-buffer or **field-scoped**. While a *writable* view
//! exists, the affected scope is conservatively treated as dirty at every
//! observation — a live view never silently receives weaker semantics.
//! Ranged writes ([`RecordBuffer::write_range`]) are the precise-dirty
//! opt-in; per-field dirty spans accumulate for the render IR.
//!
//! **Layer 3 — the render mirrors** ([`MirrorSet`]): lazy, revisioned,
//! lane-major (struct-of-arrays) copies of individual fields. Hot loops
//! read mirrors, never the interleaved buffer; a mirror rematerializes only
//! when its field's revision moved, its storage generation changed, or a
//! writable view forces conservative refresh. AoSoA layouts sized to the
//! SIMD tier derive from these mirrors in Lumen (§10.8, §17.3).

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

/// One field of the interleaved record (name + f32 lane count).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSpec {
    /// Field name (`"point"`, `"stroke_rgba"`, … or a user-declared name).
    pub name: String,
    /// Number of f32 lanes.
    pub width: usize,
}

/// The record layout plus the alignment/pointlike key sets the family
/// machinery consumes. Custom user dtypes construct these directly — a
/// schema is data, not code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSchema {
    fields: Vec<FieldSpec>,
    stride: usize,
    aligned_keys: Vec<String>,
    pointlike_keys: Vec<String>,
}

impl RecordSchema {
    /// A schema from `(name, width)` pairs, with explicit aligned and
    /// pointlike key sets.
    #[must_use]
    pub fn new(fields: &[(&str, usize)], aligned_keys: &[&str], pointlike_keys: &[&str]) -> Self {
        let fields: Vec<FieldSpec> = fields
            .iter()
            .map(|(name, width)| FieldSpec {
                name: (*name).to_string(),
                width: *width,
            })
            .collect();
        let stride = fields.iter().map(|f| f.width).sum();
        Self {
            fields,
            stride,
            aligned_keys: aligned_keys.iter().map(|k| (*k).to_string()).collect(),
            pointlike_keys: pointlike_keys.iter().map(|k| (*k).to_string()).collect(),
        }
    }

    /// The Reference's `Mobject.data_dtype`:
    /// `[('point', f32, 3), ('rgba', f32, 4)]`,
    /// `aligned_data_keys = pointlike_data_keys = ['point']`.
    #[must_use]
    pub fn mobject() -> Self {
        Self::new(&[("point", 3), ("rgba", 4)], &["point"], &["point"])
    }

    /// The Reference's `VMobject.data_dtype`, field for field.
    #[must_use]
    pub fn vmobject() -> Self {
        Self::new(
            &[
                ("point", 3),
                ("stroke_rgba", 4),
                ("stroke_width", 1),
                ("joint_angle", 1),
                ("fill_rgba", 4),
                ("base_normal", 3),
                ("fill_border_width", 1),
            ],
            &["point"],
            &["point"],
        )
    }

    /// f32 lanes per record. `stride * 4` is the NumPy itemsize.
    #[must_use]
    pub fn stride(&self) -> usize {
        self.stride
    }

    #[must_use]
    pub fn fields(&self) -> &[FieldSpec] {
        &self.fields
    }

    /// `aligned_data_keys` — the fields the family-alignment machinery
    /// null-pads together.
    #[must_use]
    pub fn aligned_keys(&self) -> &[String] {
        &self.aligned_keys
    }

    /// `pointlike_data_keys` — the fields point transforms apply to.
    #[must_use]
    pub fn pointlike_keys(&self) -> &[String] {
        &self.pointlike_keys
    }

    fn index_of(&self, field: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.name == field)
    }

    /// Lane offset of `field` within a record (NumPy byte offset ÷ 4 —
    /// numpy packs all-f32 subarray dtypes contiguously, locked by test).
    #[must_use]
    pub fn offset(&self, field: &str) -> Option<usize> {
        let mut off = 0;
        for f in &self.fields {
            if f.name == field {
                return Some(off);
            }
            off += f.width;
        }
        None
    }

    /// Lane count of `field`.
    #[must_use]
    pub fn field_width(&self, field: &str) -> Option<usize> {
        self.fields
            .iter()
            .find(|f| f.name == field)
            .map(|f| f.width)
    }
}

/// Inclusive dirty span of record indices, per field, since last take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirtySpan {
    min: usize,
    max: usize,
}

/// One fixed-capacity storage generation (V1).
#[derive(Debug)]
struct Storage {
    cells: RwLock<Box<[f32]>>,
    /// V4: bumped on every write through any path.
    revision: AtomicU64,
    /// V4, per field: bumped when that field is written.
    field_revisions: Vec<AtomicU64>,
    /// Live views attached to this generation (any kind).
    views: AtomicUsize,
    /// Writable whole-buffer views (forces conservative refresh of every
    /// field while nonzero).
    writable_whole_views: AtomicUsize,
    /// Writable field-scoped views, per field.
    writable_field_views: Vec<AtomicUsize>,
    /// Per-field dirty spans (record indices) since last take.
    dirty_spans: Mutex<Vec<Option<DirtySpan>>>,
}

impl Storage {
    fn new(cells: Box<[f32]>, n_fields: usize) -> Arc<Self> {
        Arc::new(Self {
            cells: RwLock::new(cells),
            revision: AtomicU64::new(0),
            field_revisions: (0..n_fields).map(|_| AtomicU64::new(0)).collect(),
            views: AtomicUsize::new(0),
            writable_whole_views: AtomicUsize::new(0),
            writable_field_views: (0..n_fields).map(|_| AtomicUsize::new(0)).collect(),
            dirty_spans: Mutex::new(vec![None; n_fields]),
        })
    }

    fn copy_cells(&self) -> Box<[f32]> {
        self.cells.read().expect("storage lock poisoned").clone()
    }

    fn mark_written(&self, field_index: usize, first_record: usize, last_record: usize) {
        self.revision.fetch_add(1, Ordering::AcqRel);
        self.field_revisions[field_index].fetch_add(1, Ordering::AcqRel);
        let mut spans = self.dirty_spans.lock().expect("span lock poisoned");
        let span = &mut spans[field_index];
        *span = Some(match span {
            Some(s) => DirtySpan {
                min: s.min.min(first_record),
                max: s.max.max(last_record),
            },
            None => DirtySpan {
                min: first_record,
                max: last_record,
            },
        });
    }
}

/// Interleaved f32 records under the view protocol.
#[derive(Debug)]
pub struct RecordBuffer {
    schema: Arc<RecordSchema>,
    storage: Arc<Storage>,
    len: usize,
    /// `locked_data_keys` (field indices): the copy-elision markers the
    /// animation engine consults (`lock_matching_data` skips these in
    /// `interpolate`, fm-cye). State only — locking never gates access.
    locked: HashSet<usize>,
}

impl RecordBuffer {
    #[must_use]
    pub fn new(schema: RecordSchema, len: usize) -> Self {
        let stride = schema.stride();
        let n_fields = schema.fields().len();
        Self {
            schema: Arc::new(schema),
            storage: Storage::new(vec![0.0; len * stride].into_boxed_slice(), n_fields),
            len,
            locked: HashSet::new(),
        }
    }

    /// Number of records.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[must_use]
    pub fn schema(&self) -> &RecordSchema {
        &self.schema
    }

    /// Global storage revision (V4).
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.storage.revision.load(Ordering::Acquire)
    }

    /// Per-field revision (V4) — what the lazy mirrors compare.
    #[must_use]
    pub fn field_revision(&self, field: &str) -> Option<u64> {
        let index = self.schema.index_of(field)?;
        Some(self.storage.field_revisions[index].load(Ordering::Acquire))
    }

    /// Stable identity of the current storage generation. Test hook for
    /// CoW / detach / O(touched) assertions; mirrors also use it to detect
    /// generation swaps.
    #[must_use]
    pub fn storage_id(&self) -> usize {
        Arc::as_ptr(&self.storage) as usize
    }

    /// Live views attached to the current generation.
    #[must_use]
    pub fn live_view_count(&self) -> usize {
        self.storage.views.load(Ordering::Acquire)
    }

    /// Whether a writable whole-buffer view is attached (conservative
    /// invalidation: every field counts as dirty while one exists).
    #[must_use]
    pub fn has_writable_whole_view(&self) -> bool {
        self.storage.writable_whole_views.load(Ordering::Acquire) > 0
    }

    /// Whether a writable view scoped to `field` is attached.
    #[must_use]
    pub fn field_has_writable_view(&self, field: &str) -> bool {
        self.schema
            .index_of(field)
            .is_some_and(|i| self.storage.writable_field_views[i].load(Ordering::Acquire) > 0)
    }

    fn shared_beyond_views(&self) -> bool {
        // Snapshot clones hold Arcs but never register as views; any strong
        // count beyond ourselves + live views is snapshot sharing.
        Arc::strong_count(&self.storage) > 1 + self.live_view_count()
    }

    /// V5: clone the generation if any snapshot holds it. Live views never
    /// trigger this — they are supposed to see writes. Counters and dirty
    /// spans carry over so mirror laziness stays correct across unshares.
    fn unshare(&mut self) {
        if self.shared_beyond_views() {
            let n_fields = self.schema.fields().len();
            let fresh = Storage::new(self.storage.copy_cells(), n_fields);
            fresh.revision.store(
                self.storage.revision.load(Ordering::Acquire),
                Ordering::Release,
            );
            for (new, old) in fresh
                .field_revisions
                .iter()
                .zip(self.storage.field_revisions.iter())
            {
                new.store(old.load(Ordering::Acquire), Ordering::Release);
            }
            *fresh.dirty_spans.lock().expect("span lock poisoned") = self
                .storage
                .dirty_spans
                .lock()
                .expect("span lock poisoned")
                .clone();
            self.storage = fresh;
        }
    }

    // ------------------------------------------------------------- access

    /// Read `field` of record `index`.
    #[must_use]
    pub fn read(&self, index: usize, field: &str) -> Option<Vec<f32>> {
        let off = self.schema.offset(field)?;
        let width = self.schema.field_width(field)?;
        if index >= self.len {
            return None;
        }
        let cells = self.storage.cells.read().expect("storage lock poisoned");
        let start = index * self.schema.stride() + off;
        Some(cells[start..start + width].to_vec())
    }

    /// Extract the whole column of `field`, record-major
    /// (`len * width` values) — the observation the mirror coherence tests
    /// compare against.
    #[must_use]
    pub fn read_column(&self, field: &str) -> Option<Vec<f32>> {
        let off = self.schema.offset(field)?;
        let width = self.schema.field_width(field)?;
        let stride = self.schema.stride();
        let cells = self.storage.cells.read().expect("storage lock poisoned");
        let mut out = Vec::with_capacity(self.len * width);
        for record in 0..self.len {
            let start = record * stride + off;
            out.extend_from_slice(&cells[start..start + width]);
        }
        Some(out)
    }

    /// Write `field` of record `index` (unsharing from snapshots first,
    /// V5), bumping global + field revisions (V4) and the dirty span.
    pub fn write(&mut self, index: usize, field: &str, values: &[f32]) -> bool {
        let Some(field_index) = self.schema.index_of(field) else {
            return false;
        };
        let off = self.schema.offset(field).expect("field exists");
        let width = self.schema.field_width(field).expect("field exists");
        if index >= self.len || values.len() != width {
            return false;
        }
        self.unshare();
        {
            let mut cells = self.storage.cells.write().expect("storage lock poisoned");
            let start = index * self.schema.stride() + off;
            cells[start..start + width].copy_from_slice(values);
        }
        self.storage.mark_written(field_index, index, index);
        true
    }

    /// Ranged write — the precise-dirty opt-in (`edit_points(range, …)`
    /// class): write `values` (a multiple of the field width) into
    /// consecutive records starting at `first_record`.
    pub fn write_range(&mut self, field: &str, first_record: usize, values: &[f32]) -> bool {
        let Some(field_index) = self.schema.index_of(field) else {
            return false;
        };
        let off = self.schema.offset(field).expect("field exists");
        let width = self.schema.field_width(field).expect("field exists");
        if width == 0 || !values.len().is_multiple_of(width) {
            return false;
        }
        let n_records = values.len() / width;
        if n_records == 0 {
            return true;
        }
        if first_record + n_records > self.len {
            return false;
        }
        self.unshare();
        {
            let mut cells = self.storage.cells.write().expect("storage lock poisoned");
            let stride = self.schema.stride();
            for (k, chunk) in values.chunks_exact(width).enumerate() {
                let start = (first_record + k) * stride + off;
                cells[start..start + width].copy_from_slice(chunk);
            }
        }
        self.storage
            .mark_written(field_index, first_record, first_record + n_records - 1);
        true
    }

    /// Take (and clear) the dirty span of `field`: the inclusive record
    /// range written since the last take. Feeds §10.8's dirty bounds.
    pub fn take_dirty_span(&mut self, field: &str) -> Option<(usize, usize)> {
        let index = self.schema.index_of(field)?;
        let mut spans = self.storage.dirty_spans.lock().expect("span lock poisoned");
        spans[index].take().map(|s| (s.min, s.max))
    }

    // ------------------------------------------------------------ locking

    /// `lock_data`: mark fields as unchanged for the current animation so
    /// interpolation and mirror sync can skip them (copy-elision). Pure
    /// state — access is never gated. (The `has_updaters` guard lives at
    /// the mobject layer with fm-yra, as in the Reference.)
    pub fn lock_data<'k>(&mut self, keys: impl IntoIterator<Item = &'k str>) {
        self.locked = keys
            .into_iter()
            .filter_map(|k| self.schema.index_of(k))
            .collect();
    }

    /// `unlock_data`.
    pub fn unlock_data(&mut self) {
        self.locked.clear();
    }

    /// Whether `field` is currently locked.
    #[must_use]
    pub fn is_locked(&self, field: &str) -> bool {
        self.schema
            .index_of(field)
            .is_some_and(|i| self.locked.contains(&i))
    }

    /// Locked field names, in schema order.
    #[must_use]
    pub fn locked_keys(&self) -> Vec<&str> {
        self.schema
            .fields()
            .iter()
            .enumerate()
            .filter(|(i, _)| self.locked.contains(i))
            .map(|(_, f)| f.name.as_str())
            .collect()
    }

    // ------------------------------------------------------------- resize

    /// Copy-on-resize (V1/V3): fresh generation, prefix copied, growth
    /// **null-padded** (the family-alignment padding primitive);
    /// outstanding views keep the old generation.
    pub fn resize(&mut self, new_len: usize) {
        let stride = self.schema.stride();
        let mut cells = vec![0.0f32; new_len * stride];
        {
            let old = self.storage.cells.read().expect("storage lock poisoned");
            let keep = old.len().min(cells.len());
            cells[..keep].copy_from_slice(&old[..keep]);
        }
        self.swap_in(cells);
        self.len = new_len;
    }

    /// `resize_with_interpolation`, ported from
    /// `manimlib/utils/iterables.py`: same length → no-op; a single record
    /// or an all-equal buffer repeats; zero target empties; otherwise each
    /// new record linearly interpolates its two bracketing old records
    /// (`cont_indices = linspace(0, len-1, new_len)`). Applies to every
    /// lane of every field, which equals the Reference's per-field
    /// application over the whole dtype.
    pub fn resize_with_interpolation(&mut self, new_len: usize) {
        if new_len == self.len {
            return;
        }
        let stride = self.schema.stride();
        let old = self.storage.copy_cells();
        let is_constant = self.len > 0
            && old
                .chunks_exact(stride)
                .all(|record| record == &old[..stride]);
        let mut cells = vec![0.0f32; new_len * stride];
        if self.len == 1 || is_constant {
            for record in cells.chunks_exact_mut(stride) {
                record.copy_from_slice(&old[..stride]);
            }
        } else if new_len > 0 && self.len > 0 {
            let n = self.len;
            let step = (n - 1) as f64
                / if new_len > 1 {
                    (new_len - 1) as f64
                } else {
                    1.0
                };
            for (i, record) in cells.chunks_exact_mut(stride).enumerate() {
                let ci = i as f64 * step;
                let lh = ci as usize;
                let rh = (ci.ceil() as usize).min(n - 1);
                let a = (ci.rem_euclid(1.0)) as f32;
                let left = &old[lh * stride..(lh + 1) * stride];
                let right = &old[rh * stride..(rh + 1) * stride];
                for (lane, slot) in record.iter_mut().enumerate() {
                    *slot = (1.0 - a) * left[lane] + a * right[lane];
                }
            }
        }
        self.swap_in(cells);
        self.len = new_len;
    }

    fn swap_in(&mut self, cells: Vec<f32>) {
        let n_fields = self.schema.fields().len();
        let fresh = Storage::new(cells.into_boxed_slice(), n_fields);
        // A new generation invalidates everything: bump every revision past
        // the old ones so mirrors resync.
        fresh.revision.store(
            self.storage.revision.load(Ordering::Acquire) + 1,
            Ordering::Release,
        );
        for (new, old) in fresh
            .field_revisions
            .iter()
            .zip(self.storage.field_revisions.iter())
        {
            new.store(old.load(Ordering::Acquire) + 1, Ordering::Release);
        }
        self.storage = fresh;
    }

    // -------------------------------------------------------------- views

    /// Export a live whole-buffer view — where fmn-python's zero-copy NumPy
    /// structured export attaches (V2). Unshares from snapshots first (V5).
    pub fn export_view(&mut self, writable: bool) -> RecordView {
        self.export_view_inner(writable, None)
    }

    /// Export a live view scoped to one field (the NumPy single-field view:
    /// `mobject.data["points"]`-class). Conservative invalidation then
    /// applies to that field only.
    pub fn export_field_view(&mut self, field: &str, writable: bool) -> Option<RecordView> {
        let index = self.schema.index_of(field)?;
        Some(self.export_view_inner(writable, Some(index)))
    }

    fn export_view_inner(&mut self, writable: bool, field: Option<usize>) -> RecordView {
        self.unshare();
        self.storage.views.fetch_add(1, Ordering::AcqRel);
        if writable {
            match field {
                Some(index) => {
                    self.storage.writable_field_views[index].fetch_add(1, Ordering::AcqRel);
                }
                None => {
                    self.storage
                        .writable_whole_views
                        .fetch_add(1, Ordering::AcqRel);
                }
            }
        }
        RecordView {
            schema: Arc::clone(&self.schema),
            storage: Arc::clone(&self.storage),
            len: self.len,
            writable,
            field,
        }
    }

    // ----------------------------------------------------------- clones

    /// Snapshot clone (V5): share the generation unless live views force an
    /// eager copy.
    #[must_use]
    pub fn snapshot_clone(&self) -> Self {
        if self.live_view_count() > 0 {
            let n_fields = self.schema.fields().len();
            Self {
                schema: Arc::clone(&self.schema),
                storage: Storage::new(self.storage.copy_cells(), n_fields),
                len: self.len,
                locked: self.locked.clone(),
            }
        } else {
            Self {
                schema: Arc::clone(&self.schema),
                storage: Arc::clone(&self.storage),
                len: self.len,
                locked: self.locked.clone(),
            }
        }
    }

    /// Independent deep copy (mobject `copy()` semantics, §8.3).
    #[must_use]
    pub fn deep_clone(&self) -> Self {
        let n_fields = self.schema.fields().len();
        Self {
            schema: Arc::clone(&self.schema),
            storage: Storage::new(self.storage.copy_cells(), n_fields),
            len: self.len,
            locked: self.locked.clone(),
        }
    }

    /// Reference `set_data`, the `become` data plane (§8.3): replace this
    /// buffer's records with an independent copy of `other`'s. Schemas must
    /// match — the Reference asserts dtype equality — else this returns
    /// `false` and leaves the buffer untouched. Flows through the
    /// copy-on-resize path: a fresh generation with every revision bumped
    /// past the old ones, so mirrors and bounding-box signatures resync and
    /// outstanding views detach exactly as under resize (V6).
    pub fn assign_from(&mut self, other: &RecordBuffer) -> bool {
        if !(Arc::ptr_eq(&self.schema, &other.schema) || *self.schema == *other.schema) {
            return false;
        }
        let cells = other.storage.copy_cells().into_vec();
        self.swap_in(cells);
        self.len = other.len;
        true
    }
}

/// A live view over one storage generation — the engine-side model of the
/// exported NumPy (structured or single-field) array. Dropping it unpins
/// the generation.
#[derive(Debug)]
pub struct RecordView {
    schema: Arc<RecordSchema>,
    storage: Arc<Storage>,
    len: usize,
    writable: bool,
    /// `Some(i)` for a field-scoped view.
    field: Option<usize>,
}

impl RecordView {
    /// Number of records visible to this view (fixed at export).
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether this view still aliases `buffer`'s current generation
    /// (false once a resize or restore has swapped generations, V3/V6).
    #[must_use]
    pub fn is_attached_to(&self, buffer: &RecordBuffer) -> bool {
        Arc::ptr_eq(&self.storage, &buffer.storage)
    }

    fn allows(&self, field_index: usize) -> bool {
        self.field.is_none_or(|scoped| scoped == field_index)
    }

    #[must_use]
    pub fn read(&self, index: usize, field: &str) -> Option<Vec<f32>> {
        let field_index = self.schema.index_of(field)?;
        if !self.allows(field_index) || index >= self.len {
            return None;
        }
        let off = self.schema.offset(field)?;
        let width = self.schema.field_width(field)?;
        let cells = self.storage.cells.read().expect("storage lock poisoned");
        let start = index * self.schema.stride() + off;
        Some(cells[start..start + width].to_vec())
    }

    /// Write through the view: visible to the engine while attached, and it
    /// marks render state dirty via the revisions (V4). Read-only views and
    /// out-of-scope fields return `false`.
    pub fn write(&self, index: usize, field: &str, values: &[f32]) -> bool {
        if !self.writable {
            return false;
        }
        let Some(field_index) = self.schema.index_of(field) else {
            return false;
        };
        if !self.allows(field_index) {
            return false;
        }
        let off = self.schema.offset(field).expect("field exists");
        let width = self.schema.field_width(field).expect("field exists");
        if index >= self.len || values.len() != width {
            return false;
        }
        {
            let mut cells = self.storage.cells.write().expect("storage lock poisoned");
            let start = index * self.schema.stride() + off;
            cells[start..start + width].copy_from_slice(values);
        }
        self.storage.mark_written(field_index, index, index);
        true
    }
}

impl Drop for RecordView {
    fn drop(&mut self) {
        self.storage.views.fetch_sub(1, Ordering::AcqRel);
        if self.writable {
            match self.field {
                Some(index) => {
                    self.storage.writable_field_views[index].fetch_sub(1, Ordering::AcqRel);
                }
                None => {
                    self.storage
                        .writable_whole_views
                        .fetch_sub(1, Ordering::AcqRel);
                }
            }
        }
    }
}

// ---------------------------------------------------------------- mirrors

/// One field's lane-major (SoA) mirror: `lanes[lane * len + record]`.
#[derive(Debug)]
struct FieldMirror {
    lanes: Vec<f32>,
    len: usize,
    seen_revision: u64,
    seen_storage: usize,
}

/// The lazy revisioned render mirrors (§8.2 Rev 4, D-20): what the hot
/// loops read instead of the interleaved buffer. Owned by the render side
/// (one set per renderable object), synchronized at most once per
/// observation, and only when the field actually changed.
#[derive(Debug, Default)]
pub struct MirrorSet {
    fields: std::collections::HashMap<String, FieldMirror>,
    materializations: u64,
}

impl MirrorSet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many times any field actually rematerialized — the laziness
    /// tests' observable.
    #[must_use]
    pub fn materializations(&self) -> u64 {
        self.materializations
    }

    /// Synchronize the mirror of `field` and return its lane-major data
    /// (`lanes[lane * len + record]`). Rematerializes only when the field
    /// revision moved, the storage generation swapped, or a writable view
    /// forces conservative refresh (a live view never silently receives
    /// weaker semantics). Locked fields (copy-elision) refresh like any
    /// other when their revision moves — locking is an animation-engine
    /// contract, not a mirror bypass.
    pub fn sync<'a>(&'a mut self, buffer: &RecordBuffer, field: &str) -> Option<&'a [f32]> {
        let revision = buffer.field_revision(field)?;
        let width = buffer.schema().field_width(field)?;
        let storage = buffer.storage_id();
        let forced = buffer.has_writable_whole_view() || buffer.field_has_writable_view(field);
        let stale = match self.fields.get(field) {
            Some(mirror) => {
                mirror.seen_revision != revision
                    || mirror.seen_storage != storage
                    || mirror.len != buffer.len()
                    || forced
            }
            None => true,
        };
        if stale {
            let column = buffer.read_column(field)?;
            let len = buffer.len();
            // Transpose record-major column into lane-major SoA.
            let mut lanes = vec![0.0f32; column.len()];
            for record in 0..len {
                for lane in 0..width {
                    lanes[lane * len + record] = column[record * width + lane];
                }
            }
            self.fields.insert(
                field.to_string(),
                FieldMirror {
                    lanes,
                    len,
                    seen_revision: revision,
                    seen_storage: storage,
                },
            );
            self.materializations += 1;
        }
        self.fields.get(field).map(|m| m.lanes.as_slice())
    }
}
