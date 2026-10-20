# Native pure-segment execution in the CLI

The CPU `fmn` render path now accepts Choreo's closed native pure-play and
static-wait programs. The scene owner submits nominal frame jobs; interpolation,
arena reconstruction, compilation and rasterization execute on the existing
bounded render teams. No callback, live scene, updater closure or mutable RNG
crosses that boundary. The scene runtime still owns purity classification,
begin/finish behavior, removals and the final owner state.

The cancellation adapter forwards this capability. Registered emitter handles
interrupt admission inside long segments, not just between segments. A partial
capture failure is latched, cancels the output ring before joining workers, and
cannot be finalized or retried through a serial fallback. Failed renders do not
publish partial artifacts. A new render may use the destination after cleanup.

Metal, still-PNG output and recording sinks do not opt into this CPU mechanism.
Stateful updaters, unsupported animation programs, authored rate functions and
other non-pure cases retain the ordinary scene-owner path. Fixed-camera CPU
programs use the same camera-aware pipeline; dynamic camera policies remain
with their existing front-door handling. Subdivided output remains on its
existing conservative dispatch path.

## Execution diagnostics

The robot render backend contains an additive `execution_dispatch` object with
schema `fmn.cli.dispatch`, version 1. Its counters are collected from actual
admission and validated against the completed render:

- `pure_segments` and `pure_frames`: native programs and nominal frame jobs.
- `serial_segments`: non-skipped scene segments not consumed as native programs.
- `compiled_frames`: FMTL reconstruction jobs, separately counted because their
  existing worker path is not new native interpolation offload.

The same receipt is attached to the FMNP C10 descriptive detail. These values
are not inputs to the closure digest, renderer ABI or semantic identity. Existing
frame-pipeline statistics continue to report joined worker output and outstanding
slots. A failed render cannot return a successful dispatch receipt.

```sh
fmn --robot --reproducible --format png_sequence --resolution 32x24 \
  --fps 8 --threads 4 @builtin circle_shift.v1
```

The shipping-binary regressions compare certified PNG bytes at 1, 4 and 16
threads and test animation-range rebasing and FMNP round trips. The mixed-scene
regressions compare actual serial packet execution against native jobs over
multiple admission windows, both affine and fixed-camera rendering, including
an owner-thread updater between pure plays. The Gauntlet also registers
`determinism.cli_pure_segments.v1` and produces its normal failure artifact.
These are dispatch/correctness tests, not a measured speedup claim.
