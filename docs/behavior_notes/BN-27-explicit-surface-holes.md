# BN-27: Explicit holes in native parametric surfaces

**Status:** Implemented.

## Native opt-in policy

A mathematical UV surface can have a pole at a sampled coordinate. The native
Rust surface sampler can represent an undefined sample as an explicit hole while
keeping every Stage record finite. Callers opt in with
`SurfaceSpec::try_sample_with_holes` or `ParametricSurface::build_with_holes`.

The existing native `sample`, `try_sample`, and `try_sample_with_budget` APIs
retain their strict finite-coordinate contract. Python surface admission remains
governed by BN-22.

## Geometry and errors

A non-finite position or finite-difference derivative probe marks that UV vertex
absent. Every triangle incident to an absent vertex is omitted. The full UV grid,
resolution, face order and texture coordinates remain available. Finite samples
are never clamped or moved away from a pole, and missing vertices never become
visible placeholder geometry. An entirely undefined surface draws no triangles.

Finite coordinates outside f32 storage range remain errors. Invalid controls,
dimensions and budgets are refused before sampling. A callback error propagates
at its first failure, without invoking further callbacks or publishing partial
geometry. The point/du/dv probe order is unchanged, including after a numerical
non-finite sample.

## Querying, transforming and revealing a surface

`Surface::sample_validity` returns one presence value per u-major grid row.
`points()` retains inert finite storage for a missing row; that storage is not a
position on the surface. Use the presence mask when processing raw points.
`Surface::try_uv_to_point` returns `None` when a query requires a missing sample;
the strict `uv_to_point` accessor panics in that case. An exact finite knot stays
queryable when an adjacent missing knot has zero interpolation weight.

Bounding boxes and native point-function maps ignore missing rows. Missing
normals are zero. `SurfaceMesh` splits each iso-curve into disjoint paths around
missing support while retaining the line's child identity. `TexturedSurface`
copies the mask and preserves the original normalized texture coordinates.

UV resampling marks a result absent when any contributor with positive
interpolation weight is absent. Partial reveals apply the same rule to their
collapsed boundary. A morph between finite and missing samples keeps that vertex
absent between endpoints; exactly at an endpoint its presence matches that
endpoint. This also applies to overshooting rate functions. Interpolation honors
record-field locks. Becoming a complete surface can refill previously missing
vertices without replacing the Mobject handle.

## Storage and views

All-finite surfaces retain their original record schema and sampled record bits.
A surface with holes adds the scalar `surface_validity` record field: `1` means
present and `0` means absent. Absence of the field means all vertices are present.
The existing snapshot and bundle representation carries this ordinary field;
there is no persistence-format change.

Aligning or becoming a surface with holes promotes a finite surface's schema
once. Existing exported record views retain their old generation because their
layout cannot change in place. Acquire a new view after promotion. The validity
field remains present after filling all holes, avoiding repeated layout changes.
Schema promotion preserves field defaults and locks, and all fallible family
promotions are prepared before become publishes any mutation.

## Executable witnesses

`crates/fmn-library/tests/surface_holes.rs` covers strict-API compatibility,
finite overflow, callback errors, missing triangle indices, bounding boxes,
normal/UV queries, texture coordinates, split wireframes, UV alignment, partial
reveals, field locks, exported views and native creation/morph lifecycle.

Its assembled native witness sends actual sampled holes through Stage and
`RetainedFrameRenderer`. The surrounding surface must draw while the missing
center matches an empty frame. Partial-reveal completion restores the original
frame; a morph retains the hole between endpoints and exactly matches a complete
surface at its finite endpoint. All sampled frame bytes must agree for one and
three workers.

Lumen additionally compares a masked UV grid against an independently built
triangle mesh containing only the surviving face, checks an all-missing frame
against an empty scene, and requires an explicit all-present mask to preserve
finite frame bytes exactly.
