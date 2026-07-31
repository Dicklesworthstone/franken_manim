# fuzz/ — coverage-guided fuzzing of the fmn-codec parsers (fm-ntp)

Class-`fuzz` tooling under ADR-0003: this crate is **not a workspace
member** (its own empty `[workspace]` table keeps it out of the root
build graph), it depends only on `fmn-codec` plus a pinned
`libfuzzer-sys`, its `Cargo.lock` is committed and walked by the
governed-closure check
(`crates/fmn-conformance/tests/governed_closure.rs`), and it runs as a
**scheduled CI job, never a merge gate** (`scripts/check.sh` does not
call it).

## Targets

Three coverage-guided harnesses over the §14.2 untrusted-input parsers.
Each asserts the parser's resource-budget contract (§16.5/R14): for any
input the parser must refuse with a typed error **or** succeed inside
the declared budget — never hang, never overallocate.

| Target | Entry point | Budget assertions |
|---|---|---|
| `inflate_bytes` | `fmn_codec::inflate_bytes(data, MAX_OUTPUT)` | decompressed output `<= MAX_OUTPUT` (1 MiB campaign cap) |
| `decode_png` | `fmn_codec::decode_png(data, &PngLimits{..})` | accepted `width*height <= max_pixels` (4 MP) and `rgba.len() == width*height*4`; `max_chunks = 512` |
| `decode_jpeg` | `fmn_codec::decode_jpeg(data, &JpegLimits{..})` | accepted `width*height <= max_pixels` (4 MP) and `rgba.len() == width*height*4` |

Hangs are bounded by construction (all parsers are budget-checked loops
over the input) and policed by libFuzzer's `-timeout`; allocation is
policed by the assertions above plus `-rss_limit_mb`.

## Running locally

With the cargo-fuzz toolchain (the primary, coverage-instrumented path):

```sh
cargo install cargo-fuzz --locked --version 0.13.1
cargo fuzz run inflate_bytes        # Ctrl-C to stop
cargo fuzz run decode_png   -- -max_total_time=300
cargo fuzz run decode_jpeg  -- -max_total_time=300
```

The whole campaign, exactly as CI runs it (60s per target by default;
knobs documented in the script header):

```sh
bash scripts/fuzz_scheduled.sh
FMN_FUZZ_SECONDS=600 bash scripts/fuzz_scheduled.sh   # longer session
```

If `cargo-fuzz` is not installed, the script falls back to
`cargo build --release --manifest-path fuzz/Cargo.toml --target-dir fuzz/target`
and drives the libFuzzer binaries directly — degraded guidance (no
coverage flags), but the budget assertions, hang timeout, rss cap, and
corpus replay all still hold.

## CI

`.github/workflows/ci.yml`, job `fuzz-codec`, `if: github.event_name ==
'schedule'` (the weekly Monday cron shared with the other scheduled
jobs): installs the pinned toolchain, `cargo install cargo-fuzz
--locked --version 0.13.1`, then `bash scripts/fuzz_scheduled.sh`. Any
crash fails the job; the reproducer lands in `fuzz/artifacts/` (and is
logged base64 by libFuzzer) — that is a **finding**, a real decoder bug
to fix at the source in `crates/fmn-codec`, never a reason to weaken a
harness.

## Corpus policy

- `fuzz/corpus/<target>/` is committed. Seeds are the interesting inputs
  copied from `crates/fmn-codec/tests/fixtures` (every PNG/JPEG fixture,
  plus the DEFLATE/zlib streams) and coverage-grown units from completed
  runs.
- Grow it deliberately: after a long local session, minimize before
  committing — `cargo fuzz cmin <target>` — and prefer a small corpus of
  high-coverage units over bulk.
- Crash reproducers never go in the corpus; they are fixed at the
  source, then a regression fixture lands in the fmn-codec test suite.
- `fuzz/target/` and `fuzz/artifacts/` are gitignored build/reproducer
  sinks; `fuzz/Cargo.lock` is committed and governed (class=`fuzz` rows
  in `SUITE_ALLOWLIST.tsv`).
