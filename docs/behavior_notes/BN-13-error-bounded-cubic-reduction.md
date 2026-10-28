# BN-13 — Cubic curves keep a measured error bound

**Status:** Final. Landed in W2 (fm-6cf). G1 (fm-o3j) passed on 2026-08-20; the
migration guidance below was checked against the code on 2026-10-04: the
0.1 px / `0.1 / 135` scene-unit default (`fmn-geom/src/cubic.rs`), the no-op
`set_use_simple_quadratic_approx`, `add_cubic_bezier_curve_to_with_tolerance`,
and the fallible `CubicBezier` conversions with no infallible `From`.

## What changed

Classic manim reduces an API cubic to two quadratics with no quality bound.
When `use_simple_quadratic_approx` is enabled, a shallow cubic may instead
take a one-quadratic shortcut. The selected mechanism changes point counts
and can move the curve by several output pixels.

FrankenManim has one cubic-to-quadratic converter for API cubics, smoothing,
and SVG ingestion. It emits a C1 quadratic spline and proves the
parameter-matched deviation of every piece from the source cubic using a
Bernstein convex-hull bound. The default is **0.1 output pixel**, expressed
as `0.1 / 135` scene units at the canonical 1920-pixel frame. Callers that
know another render scale can supply an explicit scene-unit tolerance.
TrueType outlines are already quadratic and pass through without loss.

The `use_simple_quadratic_approx` compatibility setting is accepted, but it
cannot bypass the bound. The audited converter emits one quadratic for
effectively quadratic input when that preserves both the bound and
shared-anchor encoding; otherwise fidelity wins over the old shortcut.

## SVG coordinates and final size

SVG cubic reduction measures its default 0.1-unit bound in the resolved
viewport, after the root viewBox, element transforms, nested viewports and
`use` placements. The importer divides the requested viewport tolerance by
a conservative maximum singular value of each full affine cascade before
calling the same converter. An anisotropic scale or skew can no longer
magnify a path-local approximation error beyond that bound.
Nonidentity transforms reserve part of the budget for publishing transformed
f64 controls. Very large coordinate offsets that cannot preserve the bound
refuse instead of silently losing precision in the affine operation.

`SVGMobject` supplies its pending height and width to the native importer.
The native paint family's actual point bounds determine the later uniform
scale, including width's precedence when both dimensions are set. Bounded
refinement checks those bounds again after subdivision changes them; the
returned cubics retain the canonical `0.1 / 135` scene-unit bound after
ordinary constructor normalization. Empty axes keep the existing no-op
resizing semantics. Invalid sizes, unrepresentable scales, converter limits
and the aggregate SVG point budget produce named refusals.

The Python portal reserves half the scene-space budget for native f32
records. Admission checks the initial records, the mandatory materialization
after `flip(RIGHT)` during orientation inspection, and a later writable
`get_points()` materialization after centering and sizing. Each phase must
preserve shared-anchor path topology. Normalization uses the actual rounded
family bounds; a requested scale below the positional API's per-call minimum
also refuses, including a small width fit after a very large height fit.
Unsized constructors retain the viewport-space approximation contract and
still check finite records and topology through ordinary flipping, centering
and writable-record access. Raw factory calls keep their returned parts in
viewport coordinates and check only the initial records. Excessive
offsets or requested dimensions can therefore produce a precision refusal;
reducing those values lets the caller preserve the stated bound.

Ordinary stationary cubic endpoints can need a representable handle
separation after an affine map or a record conversion. The shared path
representation helper repairs those accidentally collapsed real handles
without moving anchors or authored subpath-break markers. SVG publication
charges the repair against its reserved affine budget. The portal installs
a repaired f32 candidate before constructor hooks, then compares its complete
normalization with the untouched original family; the repair consumes the
same record-error budget. This includes cancellation during f64 placement
as well as f32 materialization. Six bounded repair attempts cover changing
family bounds without an unbounded refinement loop. Unsized representation
repairs have a separate raw-viewport displacement cap; they do not imply an
error guarantee for later arbitrary authored scaling.

Rust callers can use `svg_document_for_size` before their usual framing and
resizing, or `SvgDocument::parse_with_tolerance` for a known output-space
tolerance. Neither path caches an approximation independently of its size.
Authored assembly hooks still execute once and own their geometry; arbitrary
later edits or rescaling require a new tolerance decision. The cubic bound
does not replace BN-09's circle/ellipse primitive density or claim an exact
elliptical-arc-to-cubic conversion.

## Migration guidance

- Cubic point counts are adaptive. Code should treat the first and last
  points as the API anchors and should not assume that every cubic has five
  stored points.
- A scene that enabled `use_simple_quadratic_approx` may gain points and a
  more faithful curve. Remove point-count assumptions rather than attempting
  to restore the unbounded shortcut.
- Rust importers can call
  `QuadPath::add_cubic_bezier_curve_to_with_tolerance` when their output scale
  differs from the canonical frame.
- Rust `CubicBezier::build` and its `TryFrom<CubicBezier>` conversions are
  fallible. Handle `GeomError` explicitly; there is no infallible
  `From<CubicBezier> for Mobject`, because a rejected conversion must not
  publish a point-like partial object.
- The Reference's old two-quad values remain available only as conformance
  observations; they are not production output goldens.

## Why

G0-2 measured the Reference splitter at up to 7.71 pixels of deviation on its
look-study corpus. The 0.1-pixel default is 77 times tighter and sits well
inside the analytic antialiasing transition. One bound across every cubic
source makes curve quality a system property instead of a call-site accident.

Glyph outlines show the same gap. The Reference imports its TeX glyphs from
dvisvgm's cubic paths through the same two-quad split. cmex10's display
integral is 2.2 em tall. Against its Type 1 cubics, one of its 18 curves
deviates by 0.84 px at the ink oracle's scale (font_size 48 at 1080p); the
median curve deviates by 0.015 px. FrankenManim's CM faces are converted at
1/1000 em, about 0.065 px. With every glyph box equal to the Reference's
within 0.01 px, the ink oracle still measures a sliced W1 of 0.72-0.89 px on
every formula with a display integral. That residual is the Reference's
outline, so those rows cannot meet the oracle's 0.5 px bound without copying
its approximation (measured 2026-10-21, fm-4ptw).

## Evidence

- `crates/fmn-geom/tests/svg.rs`, `crates/fmn-library/tests/svg_paints.rs`,
  and `crates/fmn-python/tests/svg_lifecycle.py`: transformed cubic bounds,
  equivalent 16/1000-unit viewBoxes normalized to one scene size, budget
  refusals and authored-hook lifecycle witnesses.
- `crates/fmn-geom/src/cubic.rs`: the construction, proof, deterministic
  subdivision, degenerate handling, property tests, and bit-locked fixture
  corpus.
- `crates/fmn-geom/tests/invariant_fixtures.rs`: QuadPath routing, stationary
  endpoint encoding, compatibility-knob behavior, and smoothing invariants.
- `crates/fmn-library/src/poly.rs`: the fallible `CubicBezier` builder,
  `TryFrom` surface, and non-finite/resource-guard refusal regressions.
- `docs/g0/G0-2-look-study-ratification.md`, finding L8 and decision (f).
- `crates/fmn-geom/benches/cubic_converter.rs`: ingestion-path microbenchmark.
