# ADR-0031 — Preserve degenerate geometry and fade replacement shapes

**Status:** Accepted (application of D-05's correctness rule)
**Date:** 2026-10-31
**Bead:** fm-fuzz-reference-defects-ab96s
**Amends:** Appendix C, adding C-46 and C-47 under D-05.

## Context

Both original API-fuzzer programs reproduce on Reference
6199a00d4c1b1127ebe45cb629c3f22538b10e13.
Mobject.rescale_to_fit (mobject.py:1149–1156) divides by any nonzero
reported extent. Roundoff gives a mathematically horizontal partial Square
a small positive height, so set_height(2.02) amplifies its width into
millions of units. The magnitude depends on the numerical host.

After Arrow.become(RegularPolygon(n=4)), FadeIn copies a nine-point
target, then its starting-copy preparation calls scale(1)
(fading.py:39–43). Arrow.scale regenerates arrow geometry
(geometry.py:1202–1205, reset_points_around_ends:705–710), leaving
15 starting points against nine target points after alignment.
Interpolation raises a broadcasting error.

## Decision

C-46: a zero-height path stays unchanged under set_height; rounding noise
must not turn a horizontal partial segment into an enormous object. A
nonzero extent, including a small real extent, still rescales normally.

C-47: FadeIn after become fades the object's current replacement
geometry and style. Fade preparation must preserve compatible interpolation
topology. This ruling concerns fading; explicit Arrow geometry rebuilding
retains its own API contract.

BN-07 supplies migration guidance. These apply the existing D-05 contract;
no decision-log requirement, output-identity obligation or gate is reduced.

## Consequences

The native polygon lifecycle suite pins the original partial-square case
and ordinary/tiny nonzero controls. The native arrow lifecycle suite pins
the original replacement, exact final geometry, changing fade frames,
thread agreement and final pixels against an independent polygon.
Both existing suites run in the installed-wheel gate.

The portal already performs these cases correctly. This registration and
coverage add no runtime implementation or performance claim. The plan and
BN-07 land with this ADR; original complete quality gates remain required.
