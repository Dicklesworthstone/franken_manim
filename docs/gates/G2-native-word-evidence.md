# G2 — The Native Word evidence packet

- **Gate bead:** `fm-i1q` (flagship gate; **still OPEN** — this packet marshals
  evidence, it does not pass the gate)
- **Packet status:** **Marshaled, gate not passed** (2026-08-21)
- **Marshal:** `VioletPike` (fm-i1q.1)
- **Program owner of record:** Jeffrey Emanuel
- **Evidence source commit:** `0883b5e` (`main`, 2026-08-21)
- **Suite pin under test:** `franken_markdown @ 82588865c453b175cb1263b36e30f5b9b1941a2e`
  (`SUITE.lock:33` — fmd-font + fmd-math with span maps, extensions, tier-2)
- **Host:** Linux x86-64, kernel 6.17.0-41-generic

This is the recorded packet required by `docs/GOVERNANCE.md` §2, in the
G1 (`docs/gates/G1-core-2d-evidence.md`) shape. Unlike the G1 packet it is
**not** a pass record: two criteria are open (SVGMobject user files, and the
blocking performance gates), and the human ratification steps for the Look
Gallery text panel and the math-vs-LaTeX side-by-side review are pending.
Several recent seams are committed **code-first with batch-test pending**;
per the swarm's code-first doctrine no fresh `cargo test` run backs this
packet, and no pass below claims one.

## Gate disposition

- [ ] **PASS** — not claimable yet; see the two open rows and the pending
  ratifications in the matrix.
- [x] **OPEN** — `fm-i1q` stays open. The concrete remaining work is
  enumerated in "What still blocks G2" below.

## Acceptance matrix

The eight criteria are quoted from `fm-i1q` (plan §20.3).

| G2 criterion | Evidence | Result |
|---|---|---|
| (1) Tier-1 construct set lays out correctly and beautifully | Ratchet layout coverage 99.797 % occurrence-weighted at the pinned rev; G0-3 fmd-math ratification against TeX's published rules; `text_sample` panel rendered and verdict drafted, human ratification pending; math-formula Look Gallery side-by-side vs LaTeX references **not yet recorded** | **Partial** |
| (2) Span map drives isolate / t2c / slicing / TransformMatchingTex end-to-end | `crates/fmn-library/src/tex.rs` (isolate/t2c by source identity), `crates/fmn-anim/src/transform_matching.rs` (native span keys), portal binds `8ec3b03`/`d7fab57`/`511f7f1` — code-first, batch-test pending | **Green (code-first)** |
| (3) De-TeX'd classes native (W7DETEX) | `fm-y69` and `fm-ebl` closed; `crates/fmn-library/src/brace.rs`, `numbers.rs` (DecimalNumber), `matchers.rs`/`controls.rs` (Checkmark/Exmark, controls), matrix delimiters from the extensible-delimiter engine | **Green** |
| (4) SVGMobject works for user files (W2SVG) | Chisel processor exists and `fm-6nm` is closed (`crates/fmn-geom/src/svg.rs`), but portal `SVGMobject` is **still a structural base**: users cannot load a real `.svg` through it until `fm-5wq.4.50` lands (in progress) | **NOT YET** |
| (5) Typeset caching live (W6TEX + W8CACHE) | `fm-fw6` (fmn-cache content-addressed store) and `fm-7dw` (fmn-tex typeset caching + pre-play preflight) closed; `crates/fmn-tex/src/typeset.rs` | **Green** |
| (6) Coverage-ratchet dashboard public and live (W6RATCHET) | `docs/ratchet/dashboard.md` — frozen G0-4 denominator (9269 strings / 17711 occurrences), CI-enforced pin/ratchet lockstep, eight-rev rising trend; `fm-mol` closed | **Green** |
| (7) fmd renders `$…$` in HTML/PDF via the same crates | Same `Layout`/span-map surface serves HTML/PDF (`docs/g0/G0-3-fmd-math-ratification.md:103`); demonstration artifacts live in the `franken_markdown` repo's corpus goldens at the pinned rev, not in this tree | **Green (cross-repo citation)** |
| (8) PG-1(G2) and PG-7 enforced and blocking | Policy rows exist and are `blocking` in `docs/performance/PERF_GATES.tsv`; rig code shipped (`crates/fmn-conformance/src/perf_pg7.rs`, `perf_frontdoor.rs`, `bin/fmn-perf.rs`); Reference denominator captured (`docs/performance/reference-baseline-2026-07-28.json`); **no pinned-host observed baseline is committed — PG-1 is NOT green** | **NOT GREEN** |

## Dependency closure

Of the 15 gate blockers, 13 are closed and 2 remain open:

- **Closed:** `fm-hk9`, `fm-wgl`, `fm-ydw`, `fm-fjq`, `fm-7dw`, `fm-u1u`,
  `fm-70s`, `fm-kg9`, `fm-ebl`, `fm-fw6`, `fm-y69`, `fm-mol`, `fm-6nm`,
  plus the prerequisite gate `fm-o3j` (G1 passed 2026-08-20, `f1248b6`).
- **Open:** `fm-inr` (W10 performance rig, in progress) and — outside the
  original blocker list but binding on criterion 4 — `fm-5wq.4.50`
  (SVGMobject user files through Chisel, in progress).

## The coverage ratchet (criterion 1 numerator, criterion 6)

`docs/ratchet/dashboard.md`, computed against `franken_markdown 82588865c453`:

| Plane | Occurrence-weighted | Unique-string |
|---|---|---|
| Parse | 99.994 % | 99.989 % |
| Parse + layout | 99.797 % | 99.644 % |

- Denominator frozen at G0-4: 9269 distinct strings, 17711 occurrences,
  corpus hash `a8325e49…4bf883fc`, rules_version 1.
- Remaining blocked constructs are enumerated and tracked by name: `\dx`
  (1 occurrence, `fm-j5t`) at parse; five math-alphanumeric codepoints
  (35 occurrences total, franken_markdown Noto subset bead) at layout.
- Enforcement is structural: a `SUITE.lock` pin bump without a ratchet
  re-run fails CI, coverage decreases fail CI, and every out-of-tier
  construct must fail with its precise named tier-tagged error.
- Trend across eight pin revisions is monotone rising
  (`5310d87a` 98.916 % → `82588865` 99.797 % layout-occurrence).

Against R1's escalation path: coverage has **not** missed the checkpoint;
no public construct-sprint amendment is required at these numbers.

## Look Gallery: the `text_sample` panel (criterion 1, beauty half)

Committed at `da6ca7f` (fm-gfn):

- Panel: `docs/g0/g0-2-renders/fmn-text-sample.png`, SHA-256
  `8f175ae968fcc0aeaa30e1595b492b9d24d5d59d287362efb2346fc7c0fec143`
- Drafted verdict in `docs/g0/G0-2-look-study-ratification.md:392`:
  **different-but-fine (Behavior-Noted, ratification pending)** — the face
  divergence is the deliberate D-08/BN-05 sovereign-bundled-font call; what
  must correspond does (centring, `next_to` spacing, 60/32 size ratio, true
  italic contrast, em dash, AA edge character). Whole-frame normalized RMSE
  `0.08914188` is registered as a smoke alarm over the intentional font
  change.
- Regeneration: `g0_2_look --text-only` in `spikes/g0-8-accelerator`
  (Stage → Scribe (fmn-text over bundled Computer Modern) → production
  Lumen), fixture row added to
  `crates/fmn-conformance/fixtures/look_gallery.tsv`.

**Honest gap:** the panel's verdict awaits ratification, and the criterion's
*mathematics* half — side-by-side Look Gallery review of typeset formulas
against LaTeX-rendered references, "indistinguishable at a glance" — has no
recorded verdict sheet yet. Layout **correctness** is verified against TeX's
published rules (G0-3 ratification and the fmd-math test surface,
`crates/fmn-tex/tests/fmd_math_surface.rs`); layout **beauty** for math is
the open review.

## Span maps end-to-end (criterion 2)

- Provenance source: fm-70s closed — every glyph carries source-span
  provenance from fmd-math layout; there is no render-twice-and-align path
  anywhere in the tree.
- Native consumers: `crates/fmn-library/src/tex.rs` applies `isolate=` and
  `tex_to_color_map` by source identity through the span map;
  `crates/fmn-anim/src/transform_matching.rs` matches Scribe primitives by
  native span keys (`511f7f1`).
- Portal: `TransformMatchingTex` bound through `fmn-python` on the native
  span maps (`8ec3b03`, `d7fab57`, fm-5wq.4.49), alongside the indication
  family (fm-5wq.4.48). These binds are committed **code-first; the
  orchestrator's batch verification has not yet run over them**, so this row
  is green-by-code-and-review, not green-by-fresh-test-run.

## De-TeX'd natives (criterion 3)

`fm-y69` and `fm-ebl` closed. `Brace`/`BraceLabel` are parametric path
generators (`crates/fmn-library/src/brace.rs`), `DecimalNumber` is pure text
(`crates/fmn-library/src/numbers.rs`), Matrix brackets come from the
extensible-delimiter engine, and Checkmark/Exmark/controls are native
(`crates/fmn-library/src/matchers.rs`, `controls.rs`; portal exposure at
`9b0db8c`). None of these classes routes through a typesetter.

## SVGMobject (criterion 4) — honest NOT YET

The Chisel SVG document processor is real, hardened, and closed
(`fm-6nm`, `crates/fmn-geom/src/svg.rs`, with explicit accept/reject).
But the user-facing criterion is "SVGMobject works for user files," and
today portal `SVGMobject` is a **structural base**: the schema SVG parser
methods are unavailable callables and `SVGMobject("file.svg")` does not
construct a VMobject family from Chisel. `fm-5wq.4.50` (in progress) is the
seam that closes this. This row cannot be counted green until it lands and
is batch-verified.

## Typeset caching (criterion 5)

`fmn-cache` is the content-addressed store (`fm-fw6` closed);
`fmn-tex` owns Tex/TexText typeset caching and the pre-play preflight
(`fm-7dw` closed; `crates/fmn-tex/src/typeset.rs`, `engine.rs`). PG-7's
`formula-cached` workload is defined against a fresh cache root
(`docs/performance/PERFORMANCE_GATES.md` §canonical PG-7 workloads), so the
cache's latency claim will be measured, not asserted, when the rig runs.

## Cross-repo payoff (criterion 7)

fmd-math and fmd-font are franken_markdown workspace crates consumed here as
git dependencies at the pinned rev (`SUITE.lock:33`). The same `Layout` and
span-mapped `PlacedGlyph` surface serves franken_markdown's HTML/PDF `$…$`
rendering (`docs/g0/G0-3-fmd-math-ratification.md:103`); the demonstration
artifacts (corpus goldens) live in the `franken_markdown` repository at that
rev rather than in this tree. This packet cites, and does not duplicate,
that evidence.

## Performance gates (criterion 8) — PG-1 is NOT green

What exists at the evidence commit:

- The §17.2 policy catalog is machine-readable and marks both PG-1 rows and
  all three PG-7 rows `blocking`/`core` (`docs/performance/PERF_GATES.tsv`),
  with the explicit rule that **a row becomes pass-capable only after a
  content-addressed pinned-host observation is committed** through
  `fmn_conformance::perf::Baseline`.
- The rig code is in-tree: `crates/fmn-conformance/src/perf.rs`,
  `perf_pg7.rs`, `perf_frontdoor.rs`, `perf_host.rs`, and the
  `fmn-perf` binary. The PG-1 denominator — the Python Reference
  wall-clock — is captured in
  `docs/performance/reference-baseline-2026-07-28.json`.
- `fm-inr` (the rig on pinned profiles) is **in progress**.

What does not exist: any committed pinned-host observed baseline for
PG-1(G2) (≤ 0.5× Reference wall-clock) or PG-7 (formula < 3 ms cold /
< 100 µs cached; 10k-glyph < 20 ms). **PG-1 has no attributable pass and is
not marked green here.** Under ADR-0018, inconclusive perf evidence is not a
HOLD — but it is also not a pass, and G2 makes these gates blocking. This
row stays red until `fm-inr` commits observations on a pinned host.

## What still blocks G2

1. **PG-1(G2) and PG-7 observed on a pinned host** (`fm-inr`) — the gates
   turn blocking at G2 and currently have policy + rig but no observation.
2. **SVGMobject for user files** (`fm-5wq.4.50`) — Chisel-backed
   construction through the portal, replacing the structural base.
3. **Look Gallery ratifications** — the `text_sample` verdict
   (drafted, pending), and a recorded side-by-side math-formula review
   against LaTeX-rendered references.
4. **Batch verification** of the code-first span-map/TMT/indication portal
   binds cited above.

## Validation provenance note

This packet was assembled code-first under the swarm doctrine: no
`cargo test`, clippy, or workspace build was run for it. Every green row
above traces to a closed bead, a committed artifact with a stated hash or
path, or a named commit — and every claim that would require a fresh test
run or a human verdict is labeled as pending rather than counted. The gate
bead `fm-i1q` remains open; only the program owner's process closes it.
