# Exporting multiple portable scenes

The existing Python batch API can now capture independent FMTL artifacts:

```python
from fmn_python import RenderJob, render_scenes

report = render_scenes(
    [RenderJob("Intro", Intro), RenderJob("Orbit", Orbit)],
    "bundles",
    format="fmtl",
    bundle_camera=True,
    resolution=(960, 540),
    fps=30,
    bundle_limits={
        "max_frames": 30_000,
        "max_capture_bytes": 128 * 1024 * 1024,
        "max_output_bytes": 128 * 1024 * 1024,
    },
    continue_on_error=True,
)
```

This creates `bundles/Intro.fmtl` and `bundles/Orbit.fmtl`. A mapping or an iterable
of Scene classes, pristine instances, or `RenderJob` entries is accepted, using
the same name validation and collision rules as pixel/video batches. Constructor
arguments can be supplied through each `RenderJob.scene_kwargs`.

`bundle_camera=True` uses the native camera-aware recorder: each captured view,
light and background travels with its geometry. The default `False` retains the
planar, browser-compatible bundle. These are the same formats and the same
native writer used by single-scene `export_bundle`; the batch does not implement
another animation loop, serializer, clock, or renderer.

## Ownership, bounds and partial success

The batch planner checks every job and destination before constructing the first
scene. Recorder capabilities, explicit resolution/FPS, and bundle limits are
also checked first. `bundle_limits` accepts only `max_frames`,
`max_capture_bytes`, and `max_output_bytes`. Its values are copied and normalized
before execution, so an authored scene cannot change subsequent jobs' bounds by
mutating the caller's mapping. Limits apply independently to each scene;
`max_jobs` bounds the number of planned jobs.

Each scene runs sequentially on the calling Python thread through one
`BundleExportSession`. Native scene/proxy ownership never crosses a thread.
Exported bundles can subsequently be replayed by the standalone native engine;
exporting them does not create a Python render farm.

Publication is atomic **per artifact**, not across the batch. A later failure
never deletes a completed bundle. Fail-fast raises `BatchRenderError`, whose
`result` contains succeeded, failed and not-run outcomes. With
`continue_on_error=True`, ordinary failures are recorded and later jobs run.
KeyboardInterrupt and SystemExit always stop execution and carry progress in
`render_batch_result`. An observer failure preserves the already-published
receipt and never relabels its artifact as a failed render.

Successful outcomes contain `BundleExportResult`, including native frame/segment
counts, byte count and SHA-256. Camera outcomes identify FMTL minor 1. Neither
captured bundles nor the batch claim source-effect certification.

Pixel-output options, `threads`, partial animation selection, subdivision,
paired last-frame output, checkpoint/resume, and source certification are
explicitly unsupported in this mode. They are refused before scene construction,
rather than being ignored or written into an incompatible checkpoint receipt.
Audio remains outside the current bundle format.

## Tests

`test_bundle_batch_protocol.py` exercises batch planning, strict preflight,
immutable limits, failure ordering and ownership against an explicitly modeled
native boundary. `bundle_batching.py` runs against a real installed extension:
batched planar/camera bytes equal independent single-scene exports; exact native
byte limits and one-byte-too-small refusals are verified; swallowed capture
failures, cancellation, collision and observer exceptions preserve publication
and recovery semantics. The installed-wheel runtime gate includes this suite.
