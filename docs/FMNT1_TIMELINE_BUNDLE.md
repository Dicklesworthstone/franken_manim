# FMTL/1 — the tier-2 timeline bundle layout (AUTHORITATIVE CONTRACT, fm-oee)

**Owner decision, 2026-07-30 (GentleBeaver).** The reader (fmn-wasm
`FmnPlayer`) and the writer (the scene-side exporter) both implement EXACTLY
this layout. Drift is a bug in whichever side deviates.

**Name history.** The draft magic `FMNT` collides with
`fmn-render/src/texture.rs`'s texture schema (`Schema::new(*b"FMNT", 1, 1, 0)`) —
this bundle is `FMTL`, format version 1.

## Container

One `fmn_hash::serial` document: `Schema::new(*b"FMTL", 1, 0, 0)`. Field order
below is fixed and serialized in order. Integers are the container's canonical
little-endian; floats are IEEE-754 bits; strings are length-prefixed UTF-8;
`bytes` fields are length-prefixed octets.

## Fields (in order)

1. `engine_version: string` — the engine identity string the player refuses
   on mismatch (the same identity the certified input closure records).
2. `fps: u32` — the schedule's frame rate (MUST equal the nested plan's fps).
3. `plan: bytes` — the nested `TimelinePlan::to_bytes()` document (FMNA/5).
4. `segments: u32` — MUST equal `plan.segments().len()`.
5. Per segment entry, in authored order:
   a. `kind: u8` — `0` = pure-reconstructible, `1` = stateful (recorded).
   b. If `kind == 0`:
      - `begin: bytes` — the begin-state Stage snapshot (the scene-side
        snapshot machinery's canonical document, verbatim).
      - `end: bytes` — the end-state Stage snapshot, same form.
      - `path: u8` — the PathFunc identity (`0` = straight, then the
        `PathFunc` catalog order; the exporter's table is normative).
      - `rate: u8` — the RateFunc identity (catalog order, same rule).
      - Reconstruction law (normative): for frame alpha, the player computes
        `a = rate(alpha)` and record-interpolates begin→end exactly as
        `fmn_anim::transform::interpolate_fields` does — pointlike fields
        through `path`, every other field linear, locked fields skipped,
        computed in f64 and stored at record precision. **Export rule: the
        writer PROVES reconstructibility before marking a segment kind 0 —
        it computes one mid-segment frame through the engine and through
        record-lerp, and requires bit-identity; any segment failing the
        proof exports as kind 1 instead. Never guessed.**
   c. If `kind == 1`:
      - `frames: u32`, then per frame: `snapshot: bytes` (verbatim as above).

## Refusals (named errors, never panics)

- Malformed container → the serial reader's typed error.
- `engine_version` mismatch → `EngineMismatch { wanted, found }`.
- `fps`/`segments` disagreement with the nested plan → `PlanInconsistent`.
- Unknown `kind`/`path`/`rate` tag → `PlanInconsistent`.
- A nested plan whose total cannot fit the player's public `u32` frame-count
  surface → `FrameCountUnrepresentable` (never saturation).
- A declared segment or stateful-frame table that cannot fit the remaining
  payload's mandatory field prefixes → `PlanInconsistent`, before reservation.
- Allocator refusal while reserving a payload-validated table →
  `AllocationFailed`; loading fails without constructing a partial player.

## Determinism

Two exports of the same scene run MUST produce identical bytes: fixed field
order, canonical floats, no timestamps, no host paths, no hash-map iteration
(wherever maps appear, serialize in sorted key order).
