# The Input-Closure Specification (§16.7) — Draft 1

> Normative draft (fm-xb3, W1's "input-closure definition" deliverable).
> Status: **DRAFT** — the certified-matrix half is frozen by G0-6 (fm-zn9);
> end-to-end enforcement is G4b (fm-yp0). Until those land, this document is
> the definition every "certified" claim is tested against, and changes to it
> are reviewed like schema changes (versioned, deliberate, Gauntlet-diffed).

## 1. Purpose

`--reproducible` promises: **the complete content-hashed input closure ⇒
bit-identical raw frames, canonical PNGs, and WAV across the certified
matrix, at any thread count, forever.** A promise like that is only testable
if "input closure" is enumerated exhaustively: anything that can influence an
output bit is either *in the closure* (hashed, journaled, and reproduced) or
*proven inert* (unable to change certified bits by construction — e.g. thread
count, under the §10.5 parallelism contract). There is no third category. An
influence discovered outside the closure is a certification bug of the
highest severity: it means two runs could differ while their manifests claim
they cannot.

## 2. The enumerated closure

Every item below contributes to the **closure digest** (§4). Items marked
*(structural)* are hashed as canonical serialized documents via fmn-hash;
items marked *(bytes)* are hashed as raw byte streams.

| # | Item | Form |
|---|---|---|
| C1 | Scene sources and every transitively loaded module (Rust scene registration or Python file set as loaded by fmn-python) | bytes, per file, ordered by virtual path |
| C2 | Engine identity: the franken_manim commit (or release build id) and the full `SUITE.lock` contents | bytes |
| C3 | Toolchain: the exact pinned nightly (from `SUITE.lock`), target triple, and the SIMD build tier's target-feature set | structural |
| C4 | Configuration: the fully-resolved config **bytes** after precedence (defaults → user file → CLI), not the file paths | bytes |
| C5 | RNG seeds: the root seed and the named-substream layout version (BN-01) | structural |
| C6 | Assets and fonts: content hash of every asset and font file actually read, keyed by virtual path (bundled fonts included — bundling is not exemption) | bytes, per file |
| C7 | Execution-engine and backend identities: the semantic renderer version, execution engine (`certified` requires the certified CPU engine), and — in `standard` provenance only — annex device/driver identities | structural |
| C8 | Locale and timezone as visible to the engine (certified runs pin `C`/UTC; the pin itself is recorded) | structural |
| C9 | Capability policy: which capability traits were live (fs/process/clock/AssetFetcher implementations by identity, not by pointer) and the ffmpeg fingerprint (path + content hash + version) when the boundary is used | structural |
| C10 | The determinism mode itself (`standard` vs `certified`) and the declared certified configuration (fixed tile dims, fixed in-flight budget, etc.) | structural |

**Explicitly outside the closure (proven inert under §10.5):** thread count,
render-team topology, scheduling order, machine load, hardware identity of
the CPU (within a certified target), and wall-clock time. **Explicitly
excluded from certification (by construction):** every ffmpeg *product*;
ffmpeg's identity still enters provenance via C9.

## 3. Hashing rules

1. **One algorithm.** SHA-256 (fmn-hash's owned implementation, FIPS 180-4).
2. **Structural items** are serialized with fmn-hash's canonical Writer
   (versioned schema, defined field order, little-endian, canonicalized
   floats, trailing checksum) and hashed as those bytes. Field order changes
   are breaking (`major` bump) per D-17.
3. **Byte items** are hashed raw, then bound to their **virtual path** (the
   path the scene sees, not the host path) by hashing
   `serialize(virtual_path, content_digest)` structurally.
4. **Aggregation is ordered.** The closure digest is the SHA-256 of the
   canonical serialization of the item list, ordered by item number then by
   virtual path (byte-lexicographic). No unordered folding anywhere.
5. **Absence is encoded.** An item that does not apply (e.g. no ffmpeg
   invoked) is serialized as an explicit `absent` marker, never skipped —
   otherwise "absent" and "forgot to hash" would collide.

## 4. The sidecar provenance manifest (schema sketch)

Emitted next to every certified artifact (and, reduced, for standard runs).
Serialized both as the canonical binary document (schema family `FMNP`) and
as a human-readable text rendering. Fields:

```
manifest_version        u16.u16 (schema major.minor)
mode                    standard | certified
closure_digest          sha256          — §3.4, the headline value
items[]                 (item_id, virtual_path?, digest, detail?)
engine                  franken_manim commit/release + SUITE.lock digest
toolchain               nightly version, target triple, target-features
execution               engine id, SIMD tier, declared certified config
outputs[]               (artifact virtual path, kind, sha256)
                        kind ∈ {raw_frames, canonical_png, wav, encoded*}
                        (*encoded artifacts are listed, marked uncertified)
journal_ref?            replay-journal id when the Studio/journal is live
```

Two manifests with equal `closure_digest` and equal certified platform MUST
list identical `outputs[]` digests for certified artifact kinds. That
sentence is the whole product promise, and it is what G4b's CI enforces
across the matrix.

## 5. The certified matrix (pending G0-6)

linux-x86-64, linux-aarch64, macos-aarch64; windows-x86-64 runs functional CI
from W1 with bit-certification a separate declared decision. G0-6 (fm-zn9)
freezes this list and the certified raster arithmetic (floating vs
fixed-point); this section inherits its outcome verbatim.

## 6. Consumers

- **G4b (fm-yp0)** implements enforcement end to end.
- **W9's replay journal (fm-y7u)** records RNG substream states and content
  hashes of everything read — its hash rules are §3's, not its own.
- **W10's perf rig** keys versioned baselines by closure-relevant identity
  (engine, tier, config) using the same serialization.
- **The self-golden rig** (crates/fmn-conformance/src/golden.rs) is the
  mechanical template: content hashes via fmn-hash, per-platform lock files
  now, one certified lock once this spec's matrix half is frozen.
