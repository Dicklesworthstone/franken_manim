# ADR-0029 — G3 takes the declared CPU preview fallback; the annex requirement moves to G5

**Status:** Accepted (owner ruling 2026-10-19, owner decision session item D7,
fm-sq8.6: "CPU fallback OK")
**Date:** 2026-10-19
**Bead:** fm-sq8.6 (W5)
**Resolves:** OQ-12 (G3)
**Amends:** Plan §23 (OQ-12 entry). §20.3's G3 text already declares this
fallback and is unchanged.

## Context

§20.3 requires G3 to show "the annex serving Studio preview on supported
Apple-silicon hardware (W5's Metal backend), with a declared fallback: if the
annex misses, G3 passes on CPU preview meeting PG-3 and the annex requirement
moves to G5 with a public note". OQ-12 asked which branch G3 takes.

fm-sq8.6 recorded the state on 2026-09-23:
- Metal is real but feature-gated (`metal`) and opt-in (`render.engine=metal`).
- The live interactive Studio worker refuses non-CPU engines.
- PG-A has no target row, and its probes stay ignored until an Apple profile
  qualifies.

Nothing since has made the annex the Studio's preview engine. The
NativeFramePipeline still refuses non-CPU plans (fm-metal-route-native-render-pank).

## Decision

G3 criterion 6 is met by the declared fallback: Studio preview on the CPU
engine, meeting PG-3 on the pinned profile. The Accelerator Annex stays
`standard`-only and opt-in. It is never auto-selected and never in a certified
path (D18's refusals are unchanged).

The annex-serving-preview requirement moves to G5. This ADR is its public note.
Annex changes keep gating only on PG-A.

## Consequences

- G3 no longer waits on Metal preview. Its remaining preview obligation is PG-3
  on the CPU engine (fm-inr.1 supplies the pinned observation).
- fm-sq8.6's other two items stay open on that bead: whether macOS release
  artifacts ship with `metal`, and the Studio security review verdict. Neither
  is ruled here.
- fm-metal-route-native-render-pank (opt-in Metal for `fmn::render`) is still
  valid work. It is G5-scoped, not a G3 blocker.
- Plan §23's OQ-12 entry is trued up in the same commit.
