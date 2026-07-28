# ADR-0015 — The Python portal is a CPython-pinned, two-slot FFI boundary

**Status:** Accepted
**Date:** 2026-07-28
**Bead:** fm-aqv (the production fmn-python bridge)
**Amends:** policy under D-01 and D-03; no decision-log text changes. D-01
pre-authorizes PyO3 for fmn-python and D-03 makes fmn-python the sole
non-`forbid(unsafe_code)` crate. This ADR records the consumed version,
feature/ABI policy, and exact project-authored unsafe boundary.

## Context

G0-5 ratified the object-model crossing on Python 3.13.7 with PyO3 0.26.0 and
`abi3` disabled. It also left four production obligations to fm-aqv:
NumPy's real structured buffer skin, the shipped dependency ruling, Python
copy/deepcopy/pickle, and the complete import surface
(`docs/g0/G0-5-python-ext-ratification.md`). The first obligation is the one
that forces an FFI decision. A real zero-copy NumPy array must implement
CPython's `bf_getbuffer`/`bf_releasebuffer` slots. PyO3 exposes those slots as
`unsafe fn __getbuffer__` and `unsafe fn __releasebuffer__`; there is no safe
high-level buffer-export API in PyO3 0.26.

Marionette already supplies the lifetime proof. `RecordView` owns an
`Arc<Storage>` generation. Resizing never reallocates that generation; it
swaps a new generation into the `RecordBuffer`, leaving the old one pinned by
every outstanding view. Foreign writable views force conservative render
refresh for their whole lifetime and advance exposed field revisions when the
last view releases. The missing operation was only publishing that stable
allocation through CPython's descriptor.

The other boundary choice is packaging. Workspace tests must embed and link
CPython, while a Python extension must not link libpython in the ordinary
extension-module configuration. Making `extension-module` a default feature
would therefore make `cargo test` non-executable; enabling it only for the
extension artifact gives both modes without two manifests or a compatibility
wrapper. W11, not W10, owns the wheel/ABI matrix and namespace packaging.

## Decision

1. **fmn-python consumes exactly PyO3 0.26.0.** The root workspace pins
   `=0.26.0` with default features disabled and `macros` enabled. Its complete
   transitive graph is promoted from the G0-5-only dev rows to reviewed
   `ffi`/`build`/`runtime` rows in `SUITE_ALLOWLIST.tsv`, keyed by the same
   `(name, version)` records under ADR-0008. The obsolete `pyo3/TBD` pending
   row is retired.
2. **`abi3` stays off.** The ratified interpreter is CPython 3.13, the portal
   deliberately uses the full buffer protocol, and W11 owns any later ABI
   matrix. A future abi3 claim requires its own measurements and ADR; it is not
   an opportunistic feature toggle.
3. **`extension-module` is opt-in.** fmn-python always builds an `rlib` and
   `cdylib` named `manimlib`. Default builds link the active CPython and run the
   embedded Python acceptance suite. `--features extension-module` builds the
   importable artifact used by W11's wheel work.
4. **Project-authored unsafe is exactly two CPython buffer slots.**
   `PyRecordView::__getbuffer__` validates the descriptor pointer and writable
   request, publishes the pinned generation as bytes, and transfers one owner
   reference into `Py_buffer.obj`. `__releasebuffer__` releases only the
   format string allocated by the matching export. The crate denies
   `unsafe_op_in_unsafe_fn`, so every pointer operation remains in a reviewed
   local block. Adding any other project-authored unsafe operation to
   fmn-python requires an amendment to this ADR.
5. **The memory proof is generation lifetime plus worker confinement.**
   `RecordView` exposes a stable raw pointer only to the buffer slot. The
   owning Python class is `unsendable`; Scene and Mobject proxies are confined
   to their creating single-threaded scene worker. No Scene/Stage/RecordBuffer
   borrow is held across a Python lifecycle or updater callback. NumPy code
   must not move an exported array to another thread while the originating
   worker can access it.
6. **NumPy is a host-side portal requirement, not a Cargo dependency.**
   fmn-python imports the environment's `numpy` module to construct a packed
   all-f32 structured dtype over the exported buffer. Absence is a precise
   import/capability error. The Rust dependency closure does not add the
   third-party `numpy` crate.
7. **The pure-Python bootstrap is part of the portal, not a second engine.**
   It supplies cooperative `__init__`, live list/mapping descriptors,
   copy/deepcopy/pickle, and schema-generated modules/classes over the narrow
   Rust seam. Geometry, ownership, record generations, identity, and state
   remain in the existing Rust subsystems. The schema class DAG preserves its
   declared bases and fails module import on an unresolved base or impossible
   MRO; it never flattens a class hierarchy to make import appear green.

## Consequences

- NumPy structured arrays are genuinely zero-copy. An engine write is visible
  through the array; a NumPy write is visible to the engine; resizing detaches
  old arrays without invalidating their memory or changing the new
  generation. Because RecordBuffer is an interleaved-f32 contract, custom
  three-item dtype descriptors must name native-endian float32; other dtypes
  receive a precise `TypeError` rather than silent reinterpretation.
- The default workspace gate can execute fmn-python's real Python suite, while
  the same source builds an importable cdylib with
  `cargo build -p fmn-python --features extension-module`.
- The buffer owner deliberately presents one byte-oriented CPython buffer.
  NumPy applies the schema-derived structured dtype and record stride. No
  second layout, copy, or dtype-specific FFI implementation exists.
- Cross-thread proxy access is a defined refusal, not supported concurrency.
  Under PyO3 0.26 the `unsendable` guard raises `pyo3_runtime.PanicException`;
  fmn-python publishes the worker-thread policy explicitly. W11 must preserve
  this restriction in user-facing packaging documentation.
- The import bootstrap may create a parity-surface class whose semantic Rust
  binding has not landed yet. Such methods fail with a symbol-qualified
  `NotImplementedError`; they never silently fake geometry. The core
  Mobject/VMobject/Scene/Animation bases and their lifecycle seams are real and
  subclassable.
- W11 still owns wheels, CPython/platform ABI coverage, and the final
  `manimlib` namespace distribution policy. fm-aqv supplies the wheel-buildable
  cdylib boundary and import contract; it does not preempt that workstream.
