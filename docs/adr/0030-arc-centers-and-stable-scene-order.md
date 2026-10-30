# ADR-0030 — Keep arc centers scale independent and scene sorting stable

**Status:** Accepted (application of D-05's correctness rule)
**Date:** 2026-10-30
**Bead:** fm-fuzz-misc-geometry-hdiyw
**Amends:** Appendix C, adding C-44 and C-45 under D-05; no gate or product goal changes.

## Context

Re-execution against Reference `6199a00d` confirms two API-fuzzer findings.
`Arc.get_arc_center` calls `find_intersection`, whose 3-D absolute cutoff
depends on the fourth power of tangent length. A radius-0.08 Dot displaced
by a 3-D group reports its first anchor instead of its center. The native
helper also reproduces the failure on a small Circle translated in z.

`Scene.add` sorts using a dictionary from object identity to its last list
index. Recursive group removal can leave duplicate root entries; both then
sort at the last occurrence, moving an earlier entry past its neighbor.
The portal already preserves each entry's relative position.

## Decision

Register C-44: an arc's normal-intersection calculation uses its component's
own plane and a dimensionless parallel test. Rigid rotation carries the
center with the arc. Uniform scale and translation cannot switch its center to
the first anchor. A collapsed or numerically parallel first component keeps
the defined first-anchor fallback; insufficient points still yield no center.

Register C-45: scene z-order sorting is stable per list entry. This ruling
does not deduplicate roots or change recursive removal or family semantics.

BN-07 records both differences and migration guidance. Undefined angles of
directions below the structural extractor's existing length quantum are
measurement degeneracies, not a new product angle formula or an exclusion.

## Consequences

The native arc helper, analytic unit tests, registered translated-arc e2e
scenario and actual portal lifecycle regressions carry C-44. The portal's
nested-add regression carries C-45. No golden is regenerated, and neither
the full source gate nor current installed-wheel parity is waived. Test
locations in BN-07 identify coverage, not a claim that every gate passed.
