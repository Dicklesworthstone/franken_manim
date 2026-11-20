# ADR-0032 — Extrude actual contours and complete source families

**Status:** Accepted (application of D-05's correctness rule)
**Date:** 2026-11-20
**Amends:** Appendix C, adding C-48 under D-05.

## Context

At Reference `6199a00d4c1b1127ebe45cb629c3f22538b10e13`,
`Prismify.__init__` (`manimlib/mobject/three_dimensions.py:398–412`)
copies the entire source into its two caps, takes the source's flat
`get_anchors()` sequence, and extrudes each adjacent pair. A contour break
therefore acquires a sidewall even though it is not a geometric edge. A
point-less group has no own anchors, so its descendants appear in the two
caps without walls connecting them. The earlier native cap-reversal fix
preserved contour breaks in the far cap but did not repair these walls.

The portal already decomposes source families into pointful members before
calling the native builder. The Rust builder must offer that same complete
extrusion directly. Neither front door should connect unrelated contours.

## Decision

C-48: `Prismify` extrudes adjacent anchors only within each contour, using
Chisel's existing shared-anchor break definition. Open contours remain open;
an isolated point produces no wall. Curved edges retain the established
anchor-chord approximation; this does not introduce swept curved surfaces.

A source family produces one complete shell for each pointful member in
depth-first order. Each shell contains that member's own near cap, contour
walls, and reversed far cap. Point-less containers add no duplicate caps.
Per-member geometry and style metadata remain attached to their caps.
An ordinary leaf preserves the existing flat `[near, walls..., far]` shape.

`VGroup3D` applies its shading, joint and depth policy to its entire family,
including visible descendants under point-less groups. This follows the
Reference's recursive setters and repairs a native integration defect; it
is not a separate semantic divergence.

## Consequences

Compound glyphs, holes and disconnected shapes no longer acquire phantom
walls. Native grouped shapes gain complete shells, with the same family
organization already used by the portal. Code indexing the former two
compound caps must instead address each member shell; BN-07 explains the
migration. Native single-contour face geometry is unchanged.

Structural regression tests cover holes, open/disjoint contours, nested
families, degenerate inputs, metadata and recursive uniforms. The existing
`render_matrix.prismify_compound_cap.v1` scenario also renders the complete
shell at 1, 4 and 16 threads against independently authored walls and proves
that an intentionally inserted bridge changes the pixels. A nested raw-face
render checks that the 3D policy reaches the actual renderer.

No output-identity obligation, performance threshold or validation gate is
weakened. This ADR records the existing D-05 correctness policy's application.
