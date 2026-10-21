# Process-isolated FMTL export

The Python portal's `render_source_scenes` accepts `format="fmtl"`. Every selected
local Scene class runs in a fresh spawned host CPython process, using the same
`render_scenes` and native `BundleExportSession` as sequential bundle export.
The native standalone `fmn` executable still does not launch Python.

```python
from fmn_python import render_source_scenes

if __name__ == "__main__":
    result = render_source_scenes(
        "lessons.py", "recordings", ["Overview", "Derivation"],
        workers=2, format="fmtl", resolution=(960, 540), fps=30,
        bundle_camera=True, bundle_audio=True,
        bundle_audio_options={"audio_sample_rate": 48000, "audio_channels": 2,
                              "audio_threads": 1, "max_audio_bytes": 67108864},
        bundle_limits={"max_frames": 100000, "max_capture_bytes": 268435456,
                       "max_output_bytes": 268435456},
        checkpoint="recordings-progress.json", resume_key="lessons-and-assets-v1",
        continue_on_error=True,
    )
    print(result.as_dict())
```

Outputs are `recordings/Overview.fmtl` and `recordings/Derivation.fmtl`, published
create-only by the native exporter. Camera/audio controls and capture/mixing
bounds have the same meanings as sequential export. Audio-enabled scenes with
no cues may publish an ordinary picture-only bundle. Receipts remain typed
`BundleExportResult` values, with `BundleSoundtrackInfo` when sound is present.
Parent admission validates these modes, bounds, FPS and resolution before any
worker starts; returned receipts must agree with the admitted settings.

`workers` bounds simultaneous scene processes (1..64). Each worker has its own
capture and PCM allowance, so aggregate memory grows with concurrent scenes.
`audio_threads` separately bounds each native mixer; it is not the scene worker
count. FMTL capture does not rasterize: omit `threads`. Raster output still
defaults to one native render thread per worker. FMTL refuses raster options,
partial animation ranges, subdivision, paired PNG output and source certification
instead of ignoring them. The observed recording is not certified Python source
execution. Existing native replay/certification is a separate operation.

The parent alone owns the checkpoint. It durably records a completed receipt
before invoking `on_result`. Set `resume=True` with the same plan/key to verify
completed files and retry only unfinished scenes. Camera/audio settings, capture
limits, selected scenes, entry-source digest and working directory are bound to
the plan; worker count and timeout may change. Helpers and media versions remain
the caller's responsibility through `resume_key`. Sequential live-class plans
and process-source plans are distinct and are not interchangeable.

Fail-fast stops new admission but retains already completed publications.
Keep-going attempts later scenes after ordinary failures and timeouts; a real
worker/user cancellation still stops the batch. A timeout's cooperative cleanup
interrupt is not treated as a new user cancellation. A crash after publication
can leave an output without a complete receipt: it remains on disk and fails
no-clobber on retry, rather than being deleted or silently adopted.

## Checks

`python crates/fmn-python/tests/test_parallel_bundle_protocol.py` runs the real
spawn scheduler, JSON receipt transport, bundle admission/receipt helpers and
on-disk checkpoint verifier. The worker publishes explicit byte fixtures, not
native bundles; this is orchestration evidence, not native rendering acceptance.

`FMN_TEST_NATIVE=1 python crates/fmn-python/tests/test_parallel_bundle_native.py`
requires an installed matching wheel and exercises camera/audio scene capture,
recovery, worker-count replay and artifact hashes without a substitute renderer.
