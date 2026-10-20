# Issue #8: Python portal rendering investigation

> Evidence supplied 2026-10-20; original reconstructed workload at 640×360,
> 15 fps. Local results describe a shared development host.

## Status and scope

The supplied evidence shows FrankenManim 0.5.0's **Python portal** substantially
slower than Manim CE 0.21.0. Inspection and profiling identify repeated static
rendering and audio resampling as optimization targets. Changes add exact render/
conversion reuse, rational-phase resampling, diagnostic counters and a Python
benchmark driver.

The live published-wheel comparison reproduces the slowdown: **39.686067 seconds
for FMN versus 7.087138 for CE, or 5.60×**. The production native audio comparison
also completes: mean time fell from 4.943363 to 0.383634 seconds, with identical
complete WAV bytes. That 12.89× audio improvement does not establish a complete
Python-render improvement factor or performance-gate verdict.

The actual source-built Python portal has now completed the unchanged supplied
scene: **716 frames, 598 raw-frame reuses, 598 conversion reuses and 118
conversions**. Its complete native WAV matches the published wheel byte for
byte, and the installed retained-conversion acceptance passes at 1, 4 and 16
threads. This wheel was built from `4537d024632aa9f019e78ae4530ccf930f9bf84f`
with the **unoptimized development profile**. These are correctness and work
elimination results, not a release-speed comparison.

**Pending:** a source release-wheel comparison with the published wheel and
live CE, and complete before/after source-frame equivalence. Local release-wheel
builds were terminated in the pinned `asupersync` prerequisite before reaching
the portal. The permanent [issue #8 workflow](../../.github/workflows/python-portal-performance.yml)
builds the exact checked-out release wheel and runs the original workload through
installed Python packages. A queued job is not a passed gate. The pristine
source comparator for the separate native audio measurement remains upstream
`fb151d131d988e626bae390e083413910423ca5f`.

## Supplied evidence and workload

- [Issue #8](https://github.com/Dicklesworthstone/franken_manim/issues/8)
- [Contributor's detailed evidence comment](https://github.com/Dicklesworthstone/franken_manim/issues/8#issuecomment-6295566933)
- [Benchmark archive](https://github.com/user-attachments/files/33727698/frankenmanim-640x360-evidence.tar.gz)
- [Contributor's report](https://github.com/user-attachments/files/33727705/report.txt)

The archive contains scenes, helpers, dependency versions, harness, logs,
hardware information, audio and videos. The original project and narration were
unavailable; both engines receive the same 47-second, 220-Hz, mono 24-kHz PCM16
tone. Tone compression need not cost the same as speech compression.

The scene contains text, arrows, a timeline and arithmetic. FrankenManim uses
IBM Plex Sans with the contributor's scaling adjustment; CE uses DejaVu Sans.
The reconstructed implementations also differ in arithmetic boxing. Their
outputs are comparable but not pixel-identical. Correctness of this optimization
must therefore be checked against FrankenManim's own unchanged baseline.

For the FMN source, seven waits produce **602 static frames**, and eight plays
produce **114 animated frames**: 716 frames, or 47.733333 seconds. Static waits
account for about 84% of output frames. CE produces 714 frames, with a recorded
video duration of 47.599349 seconds. Preserve each implementation's timeline;
do not change FMN's count to make the outputs superficially match. CE's concat
output has nominal 15 fps and a slightly different average rate. Its audio
stream lasts about 47.04 seconds; FMN pads audio to its picture timeline.

### Input identities

Paths are relative to extracted `frankenmanim-640x360-20261020`, except the
archive. Hashes are SHA-256.

| Input | SHA-256 |
|---|---|
| `frankenmanim-640x360-evidence.tar.gz` | `78e62e32f9e1d3a274c1c599e09aa06b1b682823c658d36c228476f0dd0ce16e` |
| `fmn_part2.py` | `29ccd5f4e8e815aa6027a5fcfcd505f6f46159bd9f69c2eadb45ec2880ba9567` |
| `manim_part2.py` | `f63597d436b0ac235a7c1287847798ae2dfa222d1731ec5386e91124ac4901fb` |
| `stock_valuation.py` | `bf4f388525be1c3a443f021b20a67e306dc6ff9ffc5fe74731948f2401376b39` |
| `beginner_stock_valuation.py` | `54b636757b048437a4659a7eff1164244a1eb8e3b35a19e01dac247ddaddf683` |
| `audio_beginner/timed_manifest.json` | `8807e73b51a2fbd2c4f9eec8d5c204d83d33c5ef4340e7e77770353dbf7e8a64` |
| `audio_beginner/normalized/02_part2_where_value_comes_from.wav` | `29fa7b0b4eb00fcf7899f60c07e3d397d27200522f44d3cccedf67259ac8ad4b` |

## Timing evidence

The contributor used Linux Mint 22.3, a four-core Intel Xeon E5-1620 v2 and four
FMN threads. One warmup each preceded CE/FMN/FMN/CE measurements, including
startup through MP4 publication; CE animation caching was disabled.

| Contributor's engine | Mean wall time |
|---|---:|
| Manim CE 0.21.0 | 15.06 s |
| FrankenManim 0.5.0 wheel | 88.18 s |
| FMN / CE | approximately 5.86× |

### Live local published-wheel comparison

[Portable run evidence](issue-8-released-wheel-vs-ce.json) retains all observations,
commands, package/module identities, input/output hashes and FMN robot receipts.
One warmup per engine preceded three sequential measured rounds, with engine
order reversed on the second round. The separate shared Linux x86-64 worker
used CPython 3.13.16. This investigation's build jobs were stopped during the
runs; unrelated host load was not controlled. These observations cannot be
numerically equated with the contributor's Xeon results.

| Local engine | Wall run 1 | Run 2 | Run 3 | Mean |
|---|---:|---:|---:|---:|
| CE 0.21.0 | 5.892614 s | 7.419990 s | 7.948809 s | 7.087138 s |
| FMN 0.5.0 wheel | 43.126746 s | 37.227369 s | 38.704087 s | 39.686067 s |

The mean ratio is **5.599731×**. All eight outputs passed
complete decoding and stream checks: 640×360, nominal 15 fps, 714 CE or 716 FMN
frames, and stereo 48-kHz AAC starting at time zero. Inputs remained unchanged.
Within each engine, all four complete MP4 files have identical SHA-256 hashes;
the engines do not match each other. The retained individual times show the
variation in this small, unisolated sample.

Engine-specific encoding paths/defaults remain intact: container encoder tags
are `Lavf63.1.102` for CE and `Lavf60.16.100` for FMN. The package record includes
PyAV 19.0.1; system FFmpeg 6.1.1 provides FMN encoding and shared validation.
This comparison measures complete engine behavior, not equal standalone encoder
work. Profiled runs and earlier failed harness attempts are excluded.

### Original audio through the production native library

[Portable measurements and provenance](issue-8-native-audio.json) retain every run,
compiled library identity, exact driver source, compiler arguments and baseline/
optimized `sound.rs` hashes. Machine paths are replaced by relative placeholders;
host-specific metadata is omitted. The audio optimization was committed in
[`8ca300b7`](https://github.com/Dicklesworthstone/franken_manim/commit/8ca300b7).
The driver links the actual `fmn-output` and
`fmn-codec` release libraries with `--no-default-features`; both comparators
use optimization level 3 and thin LTO. This measures production native audio,
without Python rendering or FFmpeg.

One warmup each preceded three sequential, alternating-order repetitions at four
mixing threads. Wall time includes startup, input read, decode/preparation/mixing,
WAV serialization, file write and exit.

| Native audio engine | Wall run 1 | Run 2 | Run 3 | Mean |
|---|---:|---:|---:|---:|
| Pristine main `fb151d13…` | 5.142379 s | 4.750837 s | 4.936874 s | 4.943363 s |
| Optimized source | 0.368285 s | 0.411168 s | 0.371450 s | 0.383634 s |

Cue preparation, including channel mapping and resampling, fell from a mean
4.848313 to 0.293260 seconds (16.53×). The complete native audio process was
12.89× faster on this host. Warmups and additional thread checks are excluded
from these means; native `encode_ns` measures WAV serialization, not AAC/video
encoding.

All ten outputs—two warmups, six measured runs, and optimized checks at one and
16 threads—match the published 0.5.0 portal WAV oracle byte for byte:
`e0adff5a80fa8e757d3d3b63b737a065d84464ad06d2e38e1fd3307163e80bfb`.
Each contains 2,291,200 sample frames at 48 kHz, stereo S16: 47.733333 seconds,
9,164,844 WAV bytes. This covers the entire supplied audio and timeline padding.
The no-default-feature measurement does not verify the complete default-feature
Python package or replace existing PG baselines.

### Separate published-wheel profiles

These are **instrumented observations**, excluded from benchmark timing means.
The following disjoint regions sum to each profile's total. Text/Tex time is
removed from other authoring once; inclusive `label`, import, callback and
native-call observations are not added again.

| Profiled region | Original MP4 | Audio-only WAV |
|---|---:|---:|
| Static waits, including callbacks | 39.278613 s | 0.083929 s |
| Animation plays, including callbacks | 7.533253 s | 0.260910 s |
| Text and Tex constructors | 0.057597 s | 0.053171 s |
| Authored `construct` body alone | 0.000501 s | 0.000484 s |
| Other work inside `construct` | 0.022628 s | 0.018458 s |
| Native output initialization | 0.181419 s | 0.000167 s |
| Native finish | 7.763081 s | 5.882417 s |
| Outside-construct setup/import/receipt residual | 0.996195 s | 2.386212 s |
| **Total** | **55.833287 s** | **8.685747 s** |

In the MP4 profile, native `_wait` has 37.233162 seconds exclusive time and
`_play_animations` has 6.929524 seconds. The published binary exposes no finer
split of frame stepping, rendering, conversion and output interaction. Fourteen
Text constructors total 0.051461 seconds, and one Tex constructor takes
0.006135 seconds. These measure typography/geometry construction, not drawing
the same glyphs in every later frame.

Public update dispatch runs 602 times during waits (1.913517 seconds inclusive)
and 122 times during plays (0.362207 seconds, including play boundary updates).
There are 739 admission checks: 724 dispatch checks plus 15 preflight checks.
The supplied scene has no authored custom updaters; these counts do not imply
authored callback bodies ran. Inclusive import observations are 0.832880 seconds
for MP4 and 2.146879 seconds for WAV and overlap the other regions.

The working `measurements/profile-attribution-initial.json` checks the exclusive
function partition and original-workload phase partition. These instrumented
artifacts are not a committed performance baseline.

Native finish includes audio processing and output completion. FFmpeg encodes
concurrently with rendering; its final drain is only the remaining wait.
Subtracting independently profiled MP4 and WAV totals does not establish encoder
CPU time. New owner-thread counters separately expose capture/preparation,
native rendering, conversion, submission, audio mixing and finish, while robot
receipts retain exact FFmpeg argv and tool identity. Those intervals still
must not be presented as independently measured encoder CPU.

### Actual source-built Python portal

[Portable source observation](issue-8-ci-dev-source.json) records the retrieved
[CI artifact](https://github.com/Dicklesworthstone/franken_manim/actions/runs/39491324238),
wheel and extension hashes, embedded build identity, unchanged input hashes,
complete output checks and instrumented observations. A certified one-frame PNG
manifest independently identifies the compiled commit as
`4537d024632aa9f019e78ae4530ccf930f9bf84f`; a package version of `0.5.0` alone
does not distinguish this source wheel from the public release.

The original MP4 fully decodes with 716 frames and the expected audio timeline.
The native receipt reports 598 raw-frame reuses and 598 conversion reuses, leaving
118 conversions, with a single 345,600-byte conversion cache and no poisoned
tiles. The conservative regression floor is 595 reused frames: seven static
waits produce 602 frames, allowing each wait's first frame to have changed.
The observed 598 is not itself the gate threshold.
At least eight conversions are also required for the eight visible play
intervals. This coarse guard rejects a wholly frozen export; it does not replace
the moving-pixel correctness oracles in the installed acceptance fixture.

The entire source WAV is identical to the recovered published-wheel WAV, including
padding: SHA-256 `e0adff5a80fa8e757d3d3b63b737a065d84464ad06d2e38e1fd3307163e80bfb`.
The installed [retained-conversion fixture](../../crates/fmn-python/tests/retained_conversion.py)
passes cold-render oracles, external writable point views, camera/background/
membership invalidation, callback clocks and animation completion at 1, 4 and
16 threads. Separately, the [native portal job](https://github.com/Dicklesworthstone/franken_manim/actions/runs/39487933027)
at `a066063fe25a17451e298b7d71dce1c9c288b835` passed all 14 portal-video tests,
including the production retained-conversion acceptance, plus the production
native-output acceptance and registered output E2E scenario.

One development-wheel MP4 profile took 46.370 seconds of process wall time.
Its native owner-thread intervals were 2.207 seconds of capture preparation,
21.515 rendering, 2.810 conversion, 0.187 submission, 10.518 audio mixing and
1.885 final output completion. The Python profile separately reports 17.909
seconds in waits, 12.719 in plays, 0.152 in Text constructors and 0.004 in Tex.
It records 724 public update dispatches and 739 admission checks. Native and
callback intervals overlap those Python regions; do not sum the two partitions.
These development-profile observations identify executed work and do not supply
an optimized release timing or an independently measured encoding CPU cost.

The complete three-engine entry point subsequently passed one warmup, three
measured repetitions and one separate profile for each engine: **15 valid
exports**. Each engine's five complete decoded video streams match one another,
as do its decoded PCM streams. Every source export reports 598 conversion reuses
and 118 conversions. Both full native WAV exports match the same published
oracle. The final wheel-identity, media-layout and work-accounting checks were
also applied to every retained output after review; the portable record names
the frozen runner and final validator hashes. This validates orchestration and
correctness with the development wheel, not source release performance.

## Implementation and correctness argument

[`retained.rs`](../../crates/fmn-render/src/retained.rs) exposes whether the
preceding rendered pixels can be reused. The proof follows complete render
identity, including geometry, style, membership/order, placement, camera and
background. Writable external views remain observable; route changes and
failed renders retire stale evidence. Camera reuse retains the existing frame
owner rather than introducing another full-size framebuffer copy.

The [Python output path](../../crates/fmn-python/src/portal_video.rs) keeps one
bounded converted wire frame. It reuses that conversion only with a valid
unchanged-pixel proof and matching layout, then copies bytes into the next
ordered output reservation. Every original frame and timestamp is still
emitted. Scene updates, callbacks, animation completion and audio placement
remain on their existing timeline. The cache is included in the output memory
budget. Changed frames incur an extra cache copy; its complete-portal cost remains
unmeasured. Performance diagnostics are detached observations outside certified
artifact/input digests.

[`sound.rs`](../../crates/fmn-output/src/sound.rs) previously recomputed the
64-tap Blackman-windowed sinc coefficients for each output sample frame. For
rational sample rates, coefficient phases repeat after
`output_rate / gcd(input_rate, output_rate)` frames: two phases for 24→48 kHz,
160 for 44.1→48 kHz. Reuse preserves the original integer remainder, original
floating denominator, coefficient expressions, tap accumulation order and
boundary normalization. It changes neither sample count nor channel mapping.

Tables are limited to 2,048 phases (1 MiB of coefficients). Larger periods use
the original direct evaluation. Short clips also retain direct evaluation
unless table construction uses fewer coefficient evaluations than the
conservative bound `output_frames × min(input_frames, 32)`. This avoids a
16-fold increase in coefficient setup work for two-frame clips. The independent
test oracle freezes the original scalar algorithm instead of calling the new
coefficient helper.

## Verification completed and remaining

| Completed check | Result and scope |
|---|---|
| Live CE / published wheel | One warmup and three measured runs each; all eight fully decode, inputs unchanged, repeated MP4 bytes identical within each engine |
| Native release `fmn-output` `sound::tests` | 31 passed; includes independent bitwise resampling, common/coprime/extreme rates, short clips, channel mapping, placement, WAV identity and existing filter-quality tests |
| Native release `fmn-render` unit suite | 410 passed, 0 ignored; serial `--test-threads=1`, including planar identity and retained-camera checks |
| Literal production `RetainedConversion` / `convert_frame` component | 6 conversion tests passed, linked to real `fmn-frame`; compilation passed `-D warnings`. This does not exercise PyO3/session integration |
| Complete original native audio | 10 complete WAVs equal the published-wheel oracle, including 1/4/16-thread identity |
| Scoped release Clippy | Audio and renderer passed `-D warnings`; audio used `--no-default-features --lib` |
| Combined Python driver/session/provenance checks | 63 tests + 21 subtests passed: 10 driver, 27 session, 26 provenance tests |
| `test_console_rendering_protocol.py` | 48 passed |
| Workspace `cargo fmt --check` | Passed |
| Actual source-built installed portal | Original 716-frame workload fully decodes; 598 raw/conversion reuses; complete WAV matches the published oracle |
| Installed retained-conversion acceptance | Passed at 1/4/16 threads against the genuine `4537d024` development wheel |
| Native portal CI at `a066063f` | 14 portal-video tests, production native-output acceptance and registered output E2E passed |
| Original-workload entry-point contract | Six tests passed, including native-wheel identity, media-layout, static-reuse and frozen-output negative controls |
| Complete three-engine entry-point execution | 15 valid exports; repeated decoded video and PCM match within each engine; both native WAVs match the oracle; final amended checks pass on all retained outputs |

Protocol tests use controlled doubles and do not replace installed-native
execution. The new [real portal regression](../../crates/fmn-python/tests/retained_conversion.py)
compares retained output with fresh one-frame scene/renderer/converter oracles,
checks live point writes, camera/background/membership changes, callback clocks,
animation completion and conversion work counts at 1, 4 and 16 threads.
Installed-source acceptance and complete source WAV checks have now passed.
Release-wheel timing and whole-scene before/after raw-frame equivalence remain
**pending**. Full-workspace `cargo check`, `cargo clippy` and `cargo test` are
also **unpassed**; the completed native and installed tests do not establish
these gates. Both benchmark drivers' contract tests are registered in
`scripts/check.sh`.

## Reproduce through the Python front door

The [issue #8 entry point](../../scripts/benchmark_issue8.py) runs the exact
supplied workload without reconstructing it again. The caller downloads and
extracts the archive, installs dependencies and builds the source release wheel
before the benchmark. All these operations stay outside measured intervals.

```bash
python scripts/benchmark_issue8.py \
  --archive /absolute/path/frankenmanim-640x360-evidence.tar.gz \
  --input-dir /absolute/path/frankenmanim-640x360-20261020 \
  --source-python /absolute/env/source/bin/python \
  --released-python /absolute/env/released/bin/python \
  --ce-python /absolute/env/ce/bin/python \
  --source-commit FULL_SOURCE_COMMIT_SHA \
  --source-profile release \
  --source-wheel /absolute/path/source.whl \
  --output results/issue8-run1 --profile
```

The entry point verifies the archive and six scene/audio inputs, the installed
source payload against its wheel, the compiled source commit via a certified PNG
manifest, the public wheel's native extension and CE version. The source profile
is explicitly recorded; the manifest identifies the commit, not the compiler's
optimization profile, so retain the associated build log and wheel hash. Use
`--source-profile dev` for development-wheel correctness investigations. It delegates to
the general driver below for one warmup and three sequential measured rounds,
reversing engine order on alternate rounds. Separate profiles are excluded from
the means. It then checks source reuse accounting, H.264/yuv420p video and
48-kHz stereo AAC layout, complete decoded-video and PCM repeatability within
each engine, exact stream timelines and both native
WAVs against the whole-file oracle. A failure retains diagnostic evidence and
prevents a successful result. No shared-host wall-time threshold is used, and
different engines or source versions are not required to render identical pixels.

The dedicated read-only workflow builds the unchanged release profile with the
pinned toolchain and dependencies, executes actual native and installed portal
acceptance, and runs this entry point. A separate required job attempts the exact
workspace Rust gates without competing for the release job's disk or benchmark CPU.
It retains the actual wheel, build records, logs, profiles and measurements even
when a later gate fails. It neither edits source nor publishes artifacts as a
release, and it does not replace the repository's full verification policy.

Verify and extract the archive without changing scenes or audio. Install its
Cairo/Pango, FFmpeg, LaTeX/dvisvgm and font requirements. Use separate Python
environments for published, pristine-source and optimized wheels; CE may share
the published environment. Exclude installation/builds from measurements.

Use [`scripts/benchmark_python_portal.py`](../../scripts/benchmark_python_portal.py)
with a JSON configuration. Paths to interpreters and checkouts are absolute or
relative to the configuration; inputs are relative to its `cwd`. Set:

```json
{
  "cwd": "/absolute/path/frankenmanim-640x360-20261020",
  "sources": ["fmn_part2.py", "manim_part2.py", "stock_valuation.py",
              "beginner_stock_valuation.py", "audio_beginner/timed_manifest.json"],
  "audio": ["audio_beginner/normalized/02_part2_where_value_comes_from.wav"],
  "warmups": 1,
  "repetitions": 3,
  "timeout_seconds": 900,
  "engines": [{
    "label": "wheel-0.5.0",
    "python": "/absolute/env/wheel/bin/python",
    "command": ["{python}", "-m", "fmn_python", "--robot", "fmn_part2.py",
                "GetPaidThenSell", "--format", "mp4", "--resolution", "640x360",
                "--fps", "15", "--threads", "4", "--video_dir", "{output}"],
    "expected": {"width": 640, "height": 360, "fps": "15/1", "frames": 716,
                 "audio": true, "audio_duration": 47.733333}
  }]
}
```

Extend `engines` with separate pristine-source, optimized-source and CE entries,
each containing `label`, `python`, exact `command` argv, and `expected`. Use
these command arguments after each `{python}`:

| Engine | Arguments |
|---|---|
| Published wheel | `-m fmn_python --robot fmn_part2.py GetPaidThenSell --format mp4 --resolution 640x360 --fps 15 --threads 4 --video_dir {output}` |
| Pristine / optimized source | Same FMN arguments, with `--output {output}` replacing `--video_dir {output}` |
| Live CE | `-m manim --renderer cairo --disable_caching --resolution 640,360 --fps 15 --media_dir {run_dir}/media --progress_bar none manim_part2.py Part2Benchmark` |

Commands are JSON arrays, not shell strings. For every engine require
`width: 640`, `height: 360`, `fps: "15/1"` and `audio: true`. FMN expects
`frames: 716`, `audio_duration: 47.733333`; CE expects `frames: 714`,
`audio_duration: 47.04`. CE additionally sets `modules: ["manim"]` and
`artifact: "{run_dir}/media/videos/manim_part2/360p15/Part2Benchmark.mp4"`.
Source entries should specify `repository` to retain commit/status/diff identity;
that checkout record alone does not prove the installed wheel's origin.

```bash
python3 scripts/benchmark_python_portal.py comparison.json --output results/issue8-run1
python3 scripts/benchmark_python_portal.py comparison.json --output results/issue8-profile --profile
```

Use a new output directory. Engines run sequentially, reversing order on
alternating rounds; `--profile` adds separate instrumented runs. Records retain
commands, logs, package/extension identities, wall/child CPU, exits and input/
output hashes. ffprobe checks and full decoding occur outside timing. Invalid
warmups/measurements prevent successful summaries; interruptions terminate and
reap children. AAC duration checks do not replace PCM identity.

Existing [PG-8 evidence](PG8_BASELINE.md) measures binding classes, and
[`perf_frontdoor.rs`](../../crates/fmn-conformance/src/perf_frontdoor.rs) measures
the native `fmn` executable. Neither establishes acceptance for this Python
rendering workload. This investigation leaves the existing performance-gate
definitions, thresholds, goldens and release requirements unchanged.
