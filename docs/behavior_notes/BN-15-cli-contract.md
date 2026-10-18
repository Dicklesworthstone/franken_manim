# BN-15 — The CLI is a validated, capability-aware contract

**Status:** Draft. Landed in W10 (fm-iz4); becomes Final when W9's complete
front-door scenarios and G4a's source-unedited corpus pass.

## What changed

Classic manim's command line is one argparse surface whose options can imply
side effects or fail only after scene setup. FrankenManim keeps the familiar
render flags where they still have a defined meaning, but parses them through
one generated contract shared with the Parity Ledger. Invalid combinations
fail before scene work with stable exit identities. Resolution and animation
ranges are validated, alpha output is negotiated against the selected format,
and actions such as clearing the cache or revealing an output use bounded,
platform-owned operations.

The native front door also exposes capabilities that classic manim did not:

- `--robot` emits only versioned, line-oriented NDJSON.
- `--reproducible` selects the certified execution policy and canonical owned
  artifacts.
- `doctor` reports the derived execution plan and optional capabilities rather
  than discovering them during a render.
- `batch` and `studio` are explicit command scopes with their own bounded
  options.
- `--format`, `--threads`, `--ffmpeg`, `--cache-dir`, and `--math-pack` make
  formerly ambient choices part of the declared invocation.

## What stayed familiar

- `fmn render` is the default command when a scene path appears first.
- Quality presets, frame rate, background colour, scene selection, skip mode,
  GIF selection, output naming, and ffmpeg codec/pixel-format flags retain
  their recognizable meanings.
- Configuration precedence remains defaults, then the user config file, then
  explicit CLI values.
- Human help and progress remain human-facing; robot mode never mixes them
  into its data stream.

## Deliberately tiered flags

`--autoreload` is implemented by the Studio's supervised worker restart and
journal replay, not by mutating an interpreter in place. `-e/--embed` routes
through the Python front door or a Studio breakpoint; the native Rust command
does not embed an interpreter. Their stable boundaries and revisit conditions
are recorded as `OOT-CLI-AUTORELOAD` and `OOT-CLI-EMBED` in the out-of-tier
ledger rather than being described as improvements here.

Amendment (2026-10-04, fm-cli-flag-timing-ht01): a standalone file render opens
no window, so `-p/--presenter_mode` and `-f/--full_screen` (`OOT-CLI-WINDOW`)
and `--show_animation_progress`/`--leave_progress_bars` (`OOT-CLI-PROGRESS`)
are refused by name with a capability exit, as `-e` and `--autoreload` already
were. They had been accepted and ignored, and the window flags were worse than
ignored: they switched the scene clock to the Reference's 30 fps preview rate
while the file kept its configured rate, so `-f --fps 60` wrote 12 frames into a
60 fps file and `-p` dropped frames. The Studio keeps the preview semantics.

## Migration guidance

The Python portal accepts the Reference's everyday writing and quality flags:
`-w/--write_file`, `-i/--gif`, `-l/--low_quality`, `-m/--medium_quality`, `--hd`,
`--uhd`, `-r/--resolution`, and `-q/--quiet`. The pinned Reference's quality
presets set resolution only; they preserve configured fps. Explicit resolution
wins over presets, and multiple presets use the Reference's fixed low, medium,
HD, UHD order. `-w` selects MP4, or MOV with `-t`, through optional ffmpeg;
`-i` selects native GIF. `-s` saves a final PNG unless an explicit non-PNG
`--format` conflicts. Interactive `-p`, `-f` and `-e` refuse with exit 4 on file
renders. Quiet mode retains authored output, errors and robot receipts.

`fmn-python --video_dir DIRECTORY` now has the Reference's directory meaning
for both single scenes and batches. Single files use `DIRECTORY/SCENE.FORMAT`,
PNG sequences use `DIRECTORY/SCENE/frames`, and clips use
`DIRECTORY/SCENE/clips`. FMTL follows the same rule. To preserve an older
portal command's exact output file or frames/clips directory, replace its
`--video_dir PATH` with `--output PATH`. `--file_name NAME` changes one scene's
stem. `--output` and `--file_name` reject batches so selected scenes cannot
silently share a destination.

- Treat a nonzero exit as a stable category, not as arbitrary argparse text;
  robot-mode errors carry the same identity in structured output.
- Fix conflicting flags instead of relying on last-option-wins behavior. In
  particular, choose one quality or resolution selector and one compatible
  output format.
- Use `fmn doctor --robot` to inspect capabilities in automation before asking
  for optional ffmpeg output.
- Move live-reload, presenter and fullscreen workflows to `fmn studio`; use
  fmn-python or a Studio breakpoint for interactive embed points. Drop
  `-p`, `-f`, `--show_animation_progress` and `--leave_progress_bars` from file
  renders.
- Do not scrape decorated human output. Select `--robot` and consume its
  versioned NDJSON records.

## Evidence

- `API_OVERLAY.tsv`: every Reference flag, native flag, command, interaction,
  semantic status, evidence identity, test owner, and user-facing note.
- `crates/fmn-conformance/src/schema.rs` and
  `crates/fmn-conformance/tests/api_schema.rs`: fail-closed coverage, generated
  parser/Ledger artifacts, review ratchet, and Behavior Note completeness.
- `crates/fmn-cli/src/lib.rs`: parser, precedence, stable exits, human/robot
  separation, doctor, cache lifecycle, and exhaustive interaction tests.
- `docs/api/cli_flags.md`: generated user-facing flag and command tables.

## Native video-quality controls

The standalone Rust `fmn` command accepts `--crf`, `--preset`, `--tune`, and
`--video-bitrate` through the generated contract. These select compressed-video
policy independently of scene-resolution presets and imply video output.
The optional `file_writer.video_crf`, `video_preset`, `video_tune`, and
`video_bitrate` configuration keys supply defaults for video requests.

Explicit CLI fields override file settings. Selecting CRF clears a configured
bitrate and selecting bitrate clears a configured CRF; specifying both explicit
modes is a usage error. Native output formats (including `--gif`) reject the
flags. Shared Reel negotiation checks encoder compatibility before discovery
or scene execution. The resolved configuration and actual encoder argv remain
in provenance. Existing callers that supply no controls keep encoder defaults.
See [video encoding quality](../VIDEO_ENCODING_QUALITY.md) for catalogs,
examples, and the matching Rust API precedence rules.

## Long-render ffmpeg budgets

The standalone native CLI uses the render API's finite 24-hour ffmpeg watchdog
instead of the boundary's ten-minute short-job default. Its four explicit
controls are `--ffmpeg-timeout` (whole seconds), `--ffmpeg-max-log-bytes`,
`--ffmpeg-max-artifact-bytes`, and `--ffmpeg-max-input-bytes`. These select video
output, reject zero/invalid bounds and native formats, and preserve the existing
frame-count/resident-memory safeguards. The artifact cap is intersected with
the raw-input allowance. Both the robot render receipt and C9 provenance record
the admitted policy. Tool probes keep their own short deadlines. See
[native video limits](../NATIVE_VIDEO_LIMITS.md) for defaults and usage.

The Python `fmn-python` console accepts the same four flags in separate-value
or `--flag=value` form. They apply to MP4/MOV (including transparent MOV) and
compressed-audio decode for WAV output; native image/vector formats reject
them. The portal shares the native 24-hour watchdog and admission function,
keeps its existing frame-count-derived raw-input allowance, and forwards the
admitted policy through video encode, audio decode, and stream-copy mux. Numeric
and format syntax failures precede source import; native range/ownership
checks precede tool discovery and output reservation. Batch, subdivision,
paired output, live recording, and robot receipts all carry the policy.

## Python portal video-quality extension

`fmn-python` additionally accepts `--crf`, `--preset`, `--tune`, and
`--video-bitrate`, in separate-value or `--flag=value` form. These controls
select compressed-video policy, not the familiar scene-resolution quality
presets. CRF zero is retained; CRF and bitrate cannot be combined. Omitting all
four preserves prior encoder defaults. Bitrate is integer bits per second.

The Python console owner consumes only these extension flags before delegating
legacy flags to the existing native parser. Range, duplicate, malformed-value,
and native/explicit-transparent-format errors are usage failures before source
import. Reel's shared typed negotiation validates exact preset/tune catalogs
and encoder compatibility before discovery/spawn; it never ignores a requested
software-only control on a hardware encoder. These checks do not duplicate
encoder policy in Python or change D2's subprocess and publication boundaries.

The same overrides reach named batches, `--write_all`, subdivisions, and video
primaries paired with a final PNG, without injecting scene constructor kwargs
or rerunning the scene. Actual encoder argv remains in each native receipt.
See [Python video quality](../PYTHON_VIDEO_QUALITY.md) for usage and permanent
parser/extension tests. AAC-bitrate is independently configured as described below.

## AAC soundtrack bitrate

`--audio-bitrate BPS` is available in both console front doors. It sets the
second-stage AAC encoder's target in integer bits per second, independently of
CRF, video bitrate, preset and tune. `file_writer.audio_bitrate` supplies a
layered optional default; `RenderOptions.audio_quality.bitrate` overrides that
key in native Rust. Missing/None preserves ffmpeg's AAC default, not a guessed
bitrate. A target is not a promised output size or measured quality improvement.

Zero, non-integer and out-of-range targets are refused before scene execution
or encoder startup. PNG, GIF, Y4M, WAV and SVG refuse an explicit AAC CLI option;
native WAV mixing is never compressed. Alpha-preserving MOV accepts an AAC
target without changing its video codec. Python console overrides reach named
batches, subdivisions and paired-final-frame exports through the existing
writer owner. The video stream is always copied during muxing; native PCM,
frame sampling, and raw rendered pixels are unaffected.

The exact `-b:a` argument appears only on the mux invocation in provenance.
Lower-level callers use `Boundary::with_audio_quality` or
`FfmpegSink::with_audio_quality`; their existing constructors and defaults are
unchanged. Tests in `fmn-output/tests/audio_quality.rs`, the native video tests,
and the portal quality suites cover real process arguments, unchanged native
PCM, actual AAC decode, and byte-identical H.264 streams across AAC targets.
