# The Behavior Notes — the register (§16.8)

The user-facing register of deliberate differences from classic manim: one
evidence-backed note per entry, **written as migration guidance, not as a
conformance ledger**. If your manim scene behaves differently under
FrankenManim, the reason is here, and so is what to do about it.

This file is the authoritative index. Plan §16.8 seeds the register; the
register grows past its seed as workstreams land, and §16.8 is trued up when it
does (ADR-0009).

## The numbering rules

Deliberately the ADR discipline, because a Behavior Note is cited by number
from the Parity Ledger's `evidence` column, from crate documentation, and from
migration guidance users read:

1. **One number per note.** Numbers are assigned monotonically, **never
   reused**, and never renumbered once anything outside the note cites them.
2. **A number may name a family** when the plan's own register entry does. BN-07
   is the standing example: §16.8 defines it as "Reference bugs fixed (Appendix
   C rulings)", and Appendix C carries eleven rulings owned by different
   workstreams, so BN-07 is several notes under one topic rather than one
   document that would have to be rewritten by every workstream that touches it.
3. **A reserved number stays reserved.** BN-06 belongs to the renderer and is
   written when Lumen exists (W5). Filling a reserved slot with unrelated
   content is how registers rot.
4. **Status vocabulary:** `Draft` → `Final`. A note goes Final when its
   subsystem's gate passes and its migration guidance has been reviewed against
   the real behaviour, not when the code lands.

## The register

| # | Note | File | Workstream | Status |
|---|---|---|---|---|
| BN-01 | One RNG: PCG64DXSM with named substreams; seeded scenes reproduce within FrankenManim, not across engines | [BN-01-single-rng.md](BN-01-single-rng.md) | W1 | Draft |
| BN-02 | The rational clock on manim's nominal sample points — no drift | [BN-02-rational-clock.md](BN-02-rational-clock.md) | W4 | Draft |
| BN-03 | True arc length under the original names: constant-speed paths, length-true dashes and tips | [BN-03-true-arc-length.md](BN-03-true-arc-length.md) | W2 | Draft |
| BN-04 | Colour: linear-light compositing, with manim's gradient formulas kept | [BN-04-color.md](BN-04-color.md) | W1 | Draft |
| BN-05 | Native typesetting: metrics differ from LaTeX; the quality bar is documented | [BN-05-native-text-typesetting.md](BN-05-native-text-typesetting.md) | W6 | Draft |
| BN-06 | *Reserved* — the renderer: analytic coverage, round caps, arc-length stroke width | *(unwritten; W5 owns it)* | W5 | Reserved |
| BN-07 | **Reference bugs fixed** (Appendix C rulings) — a family, see rule 2 | [stroke uniforms (C-2, C-7)](BN-07-stroke-uniform-fixes.md) · [updaters and group addition (C-5, C-6)](BN-07-updater-and-group-fixes.md) | W3 | Draft |
| BN-08 | The de-TeX'd classes (§11.6): Brace, Matrix delimiters, DecimalNumber, the drawn marks | [BN-08-de-texed-natives.md](BN-08-de-texed-natives.md) | W7 | Draft |
| BN-09 | One arc-density rule, never coarser than the Reference's | [BN-09-arc-density.md](BN-09-arc-density.md) | W2/W7 | **Final** |
| BN-10 | Skip mode delivers the same updater time as playback (§9.3) | [BN-10-skip-mode-updater-time.md](BN-10-skip-mode-updater-time.md) | W4 | Draft |
| BN-11 | Composition honours its declared timing (C-10, C-11) | [BN-11-composition-timing.md](BN-11-composition-timing.md) | W4 | Draft |
| BN-12 | The Animation contract's typed edges (§9.1) | [BN-12-animation-contract.md](BN-12-animation-contract.md) | W4 | Draft |

BN-10 and BN-12 grew past §16.8's seed list, which is expected — the seed names
the differences the plan could foresee, and a workstream that finds another
deliberate divergence writes it a note rather than filing it nowhere. ADR-0009
trues §16.8 up to this table and fixes the numbering rules above so the next
addition does not need an ADR of its own.

## Citing a note

The Parity Ledger's `evidence` column carries the bare number (`BN-08`), which
is why rule 1 exists: a number that names two documents makes the ledger
ambiguous exactly where it is meant to be authoritative. Ledger cells are
generated from `API_OVERLAY.tsv`'s `[status]` section — edit the overlay, then
regenerate with

```
UPDATE_API_ARTIFACTS=1 cargo test -p fmn-conformance --test api_schema
```

Crate documentation cites notes inline (`grep -rn 'BN-0' crates/`), and those
citations are the reason a note that anything references is never renumbered.
