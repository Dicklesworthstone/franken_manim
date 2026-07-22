//! The RecordBuffer core: interleaved f32 records under the §8.2 view
//! protocol, exactly as ratified by G0-1 (V1–V6 in
//! `docs/g0/G0-1-object-model-ratification.md`).
//!
//! The protocol, stated once (the ratification note is the authority):
//!
//! - **V1** Storage generations are fixed-capacity `Arc`-owned allocations;
//!   growth is a fresh generation swapped in (copy-on-resize, null-padded).
//!   Reallocation under a live view is impossible by construction.
//! - **V2** Views pin their generation (they hold the `Arc`).
//! - **V3** Live views alias the current generation; a resize or snapshot
//!   restore detaches them — still readable, no longer tracking.
//! - **V4** Every write bumps the generation's revision counter; render
//!   state is dirty by comparison (lazy revisioned mirrors), never by
//!   eager notification.
//! - **V5** A generation is never simultaneously shared by a snapshot and
//!   aliased by a live view: writers and view-exporters unshare first;
//!   snapshots eagerly copy buffers with live views.
//! - **V6** Restore swaps generations in, so views detach as under resize.
//!
//! Still to land here (fm-cus): typed field views, custom user dtypes
//! through the schema machinery, `aligned_data_keys`/`pointlike_data_keys`,
//! data locking as copy-elision, resize-with-interpolation, null-padding
//! family alignment, and the mandatory lazy revisioned render mirrors
//! feeding §10.8's compiled render state.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

/// One field of the interleaved record (name + f32 lane count).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSpec {
    /// Field name (`"point"`, `"rgba"`, …).
    pub name: &'static str,
    /// Number of f32 lanes.
    pub width: usize,
}

/// The record layout. Custom user dtypes ride this same machinery — a
/// schema is data, not code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSchema {
    fields: Vec<FieldSpec>,
    stride: usize,
}

impl RecordSchema {
    #[must_use]
    pub fn new(fields: Vec<FieldSpec>) -> Self {
        let stride = fields.iter().map(|f| f.width).sum();
        Self { fields, stride }
    }

    /// The core VMobject layout, abridged until fm-cus lands the full
    /// field-for-field port.
    #[must_use]
    pub fn manim_default() -> Self {
        Self::new(vec![
            FieldSpec {
                name: "point",
                width: 3,
            },
            FieldSpec {
                name: "rgba",
                width: 4,
            },
        ])
    }

    /// f32 lanes per record.
    #[must_use]
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// Lane offset of `field` within a record.
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

/// One fixed-capacity storage generation (V1).
#[derive(Debug)]
struct Storage {
    cells: RwLock<Box<[f32]>>,
    /// V4: bumped on every write through any path.
    revision: AtomicU64,
    /// Live views attached to this generation.
    views: AtomicUsize,
}

impl Storage {
    fn new(cells: Box<[f32]>) -> Arc<Self> {
        Arc::new(Self {
            cells: RwLock::new(cells),
            revision: AtomicU64::new(0),
            views: AtomicUsize::new(0),
        })
    }

    fn copy_cells(&self) -> Box<[f32]> {
        self.cells.read().expect("storage lock poisoned").clone()
    }
}

/// Interleaved f32 records with the view protocol.
#[derive(Debug)]
pub struct RecordBuffer {
    schema: Arc<RecordSchema>,
    storage: Arc<Storage>,
    len: usize,
}

impl RecordBuffer {
    #[must_use]
    pub fn new(schema: RecordSchema, len: usize) -> Self {
        let stride = schema.stride();
        Self {
            schema: Arc::new(schema),
            storage: Storage::new(vec![0.0; len * stride].into_boxed_slice()),
            len,
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

    /// Current storage revision — the dirty signal the lazy render mirrors
    /// compare against (V4).
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.storage.revision.load(Ordering::Acquire)
    }

    /// Stable identity of the current storage generation. Test hook for the
    /// CoW / detach / O(touched) assertions.
    #[must_use]
    pub fn storage_id(&self) -> usize {
        Arc::as_ptr(&self.storage) as usize
    }

    /// Live views attached to the current generation.
    #[must_use]
    pub fn live_view_count(&self) -> usize {
        self.storage.views.load(Ordering::Acquire)
    }

    fn shared_beyond_views(&self) -> bool {
        // Snapshot clones hold Arcs but never register as views; any strong
        // count beyond ourselves + live views is snapshot sharing.
        Arc::strong_count(&self.storage) > 1 + self.live_view_count()
    }

    /// V5: clone the generation if any snapshot holds it. Live views never
    /// trigger this — they are supposed to see writes.
    fn unshare(&mut self) {
        if self.shared_beyond_views() {
            self.storage = Storage::new(self.storage.copy_cells());
        }
    }

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

    /// Write `field` of record `index` in place (unsharing from snapshots
    /// first, V5), bumping the revision (V4). Returns `false` on an unknown
    /// field, out-of-range index, or width mismatch.
    pub fn write(&mut self, index: usize, field: &str, values: &[f32]) -> bool {
        let Some(off) = self.schema.offset(field) else {
            return false;
        };
        let Some(width) = self.schema.field_width(field) else {
            return false;
        };
        if index >= self.len || values.len() != width {
            return false;
        }
        self.unshare();
        {
            let mut cells = self.storage.cells.write().expect("storage lock poisoned");
            let start = index * self.schema.stride() + off;
            cells[start..start + width].copy_from_slice(values);
        }
        self.storage.revision.fetch_add(1, Ordering::AcqRel);
        true
    }

    /// Copy-on-resize (V1/V3): fresh generation, prefix copied, growth
    /// null-padded; outstanding views keep the old generation.
    pub fn resize(&mut self, new_len: usize) {
        let stride = self.schema.stride();
        let mut cells = vec![0.0f32; new_len * stride];
        {
            let old = self.storage.cells.read().expect("storage lock poisoned");
            let keep = old.len().min(cells.len());
            cells[..keep].copy_from_slice(&old[..keep]);
        }
        self.storage = Storage::new(cells.into_boxed_slice());
        self.len = new_len;
    }

    /// Export a live view — the seam where fmn-python's zero-copy NumPy
    /// structured export attaches (V2). Unshares from snapshots first (V5).
    pub fn export_view(&mut self, writable: bool) -> RecordView {
        self.unshare();
        self.storage.views.fetch_add(1, Ordering::AcqRel);
        RecordView {
            schema: Arc::clone(&self.schema),
            storage: Arc::clone(&self.storage),
            len: self.len,
            writable,
        }
    }

    /// Snapshot clone (V5): share the generation unless live views force an
    /// eager copy.
    #[must_use]
    pub fn snapshot_clone(&self) -> Self {
        if self.live_view_count() > 0 {
            Self {
                schema: Arc::clone(&self.schema),
                storage: Storage::new(self.storage.copy_cells()),
                len: self.len,
            }
        } else {
            Self {
                schema: Arc::clone(&self.schema),
                storage: Arc::clone(&self.storage),
                len: self.len,
            }
        }
    }

    /// Independent deep copy (mobject `copy()` semantics, §8.3).
    #[must_use]
    pub fn deep_clone(&self) -> Self {
        Self {
            schema: Arc::clone(&self.schema),
            storage: Storage::new(self.storage.copy_cells()),
            len: self.len,
        }
    }
}

/// A live view over one storage generation — the engine-side model of the
/// exported NumPy structured array. Dropping it unpins the generation.
#[derive(Debug)]
pub struct RecordView {
    schema: Arc<RecordSchema>,
    storage: Arc<Storage>,
    len: usize,
    writable: bool,
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

    /// Write through the view: visible to the engine while attached, and it
    /// marks render state dirty via the revision (V4). Read-only views
    /// return `false`.
    pub fn write(&self, index: usize, field: &str, values: &[f32]) -> bool {
        if !self.writable {
            return false;
        }
        let Some(off) = self.schema.offset(field) else {
            return false;
        };
        let Some(width) = self.schema.field_width(field) else {
            return false;
        };
        if index >= self.len || values.len() != width {
            return false;
        }
        {
            let mut cells = self.storage.cells.write().expect("storage lock poisoned");
            let start = index * self.schema.stride() + off;
            cells[start..start + width].copy_from_slice(values);
        }
        self.storage.revision.fetch_add(1, Ordering::AcqRel);
        true
    }
}

impl Drop for RecordView {
    fn drop(&mut self) {
        self.storage.views.fetch_sub(1, Ordering::AcqRel);
    }
}
