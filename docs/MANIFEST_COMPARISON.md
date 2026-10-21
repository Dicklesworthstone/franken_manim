# Comparing certified output manifests

`fmn-manifest-compare LEFT.fmnp RIGHT.fmnp` verifies the FMNP declarations before
comparing their platform-neutral input identities and certified artifacts.
It does not open the output artifacts, authenticate a publisher, or reconstruct a
render. A comparison is evidence about two valid declarations, not a signature or
an on-disk artifact audit.

| Exit | Verdict | Meaning |
| --- | --- | --- |
| 0 | `agree` | Both modes are certified, semantic inputs agree, and at least one certified artifact is compared. Every certified path has the same kind and digest on both sides. |
| 1 | `determinism-failure` | Both modes are certified and semantic inputs agree, but a certified artifact is missing, has lost its certified flag, or differs in kind or digest. |
| 2 | `not-comparable` | Semantic inputs differ, a mode is not certified, or neither manifest declares any certified output. |
| 64 | No comparison verdict | Usage, file reading, framing, checksum, canonical ordering, or manifest-validation failure. A diagnostic is written to stderr. |

Ordinary comparison output retains the `fmn.manifest-compare` version-1 JSON
record. Comparing two certified-mode documents with only uncertified outputs
emits `not-comparable` with the additional reason `no-certified-outputs`. Such
documents remain valid provenance for encoded video, but empty evidence cannot
prove certified bit agreement. Standard-mode comparisons remain noncomparable.

Each manifest output path must be unique, including uncertified outputs. The
verifier refuses duplicate paths even when their hashes match; it never lets a
later declaration replace an earlier one. Output order does not affect the
verdict, and `differing_outputs` is sorted by the exact virtual-path spelling.
Unrelated uncertified artifacts are outside the certified bit promise.

The library's `ProvenanceManifest::validate()` rechecks structure, identified-build
requirements, and the stored input seal after public-field edits. Serialization,
semantic hashing, and comparison use that validation. The decoder refuses a
noncanonical item order rather than silently sorting an already-sealed file.
Authoring with `ProvenanceManifest::new` still sorts input items. Valid FMNP wire
bytes, schema versions, and input-digest definitions are unchanged; output kinds
and hashes remain evidence rather than inputs to the closure digest.

The command reads regular manifest files through the shared allocation-bounded
filesystem capability with a 64 MiB limit, enforced during reading. A metadata
size check followed by an unbounded whole-file read is not used.

## Regression targets

```sh
cargo test --locked -p fmn-output --lib provenance::
cargo test --locked -p fmn-output --test provenance_integrity --test manifest_compare_integrity
```

The integration targets include independently written, checksum-valid malformed
FMNP documents and execute the actual comparison binary. They distinguish
library admission, on-wire admission, and process exit behavior. Passing these
tests does not by itself establish cross-platform rendering determinism.
