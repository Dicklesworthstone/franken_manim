# BN-01 — One RNG: reproducible within FrankenManim, not across engines

**Status:** Draft (W1, fm-m1u). Finalized when the segment-purity
classifier consumes the fork API (fm-3xk).

## What changed

Classic manim consults **two** seeded legacy streams — CPython's
`random` and NumPy's legacy `RandomState` — wired through whatever import
happens to touch them. Feature changes shift unrelated draw sequences,
and "same seed" means different pictures across manim versions.

FrankenManim has exactly one stream: **PCG64DXSM seeded through NumPy's
`SeedSequence`**, bit-exact against NumPy for explicit seeds (locked by
`crates/fmn-core/fixtures/rng_vectors.txt`, generated from NumPy itself).
On top of it:

- **Named substreams** (`root.substream("stream_lines")` …): every
  subsystem draws from its own spawn-key-derived stream. Adding a
  consumer can never shift an existing consumer's draws.
- **Keyed per-frame forks**: a frame's stream is a pure function of
  `(substream, frame_index)` — never a sequential pull consumed in
  scheduler completion order. This is what makes frame-parallel
  rendering replay-identical by construction; completion-order RNG is
  permanently refused (D-18, §10.5).
- **Snapshot/restore** of full generator state feeds SceneState and the
  replay journal.
- Render-affecting map iteration uses ordered maps only
  (`fmn_core::rng::OrderedMap`; hash-map order would smuggle
  nondeterminism past the RNG discipline).

## Migration guidance

- A seeded scene reproduces **within FrankenManim** — same seed, same
  build, same bits, any thread count. It does **not** reproduce the
  Python engine's draws: the legacy streams are gone by design.
- Scenes that relied on `np.random.seed(...)` global state should pass
  the seed through scene config; the engine's substreams take care of
  isolation.
- Python scenes may still import real NumPy and draw their own numbers;
  those draws are the scene's business and are captured by the input
  closure only insofar as §16.7 documents.

## Evidence

- `crates/fmn-core/src/rng.rs` (implementation + doctrine notes),
  `crates/fmn-core/tests/rng_parity.rs` (NumPy bit-parity: SeedSequence
  words, draws, doubles, substream and fork spawn keys), unit tests for
  independence / call-order invariance / snapshot round-trip.
- Implementation detail pinned by vectors: NumPy's PCG64DXSM seeds with
  the standard 128-bit-multiplier srandom, then transitions with the DXSM
  cheap multiplier at runtime.
