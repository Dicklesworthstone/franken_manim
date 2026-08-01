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
  presenter mode, GIF selection, output naming, and ffmpeg codec/pixel-format
  flags retain their recognizable meanings.
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

## Migration guidance

- Treat a nonzero exit as a stable category, not as arbitrary argparse text;
  robot-mode errors carry the same identity in structured output.
- Fix conflicting flags instead of relying on last-option-wins behavior. In
  particular, choose one quality or resolution selector and one compatible
  output format.
- Use `fmn doctor --robot` to inspect capabilities in automation before asking
  for optional ffmpeg output.
- Move live-reload workflows to `fmn studio`; use fmn-python or a Studio
  breakpoint for interactive embed points.
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
