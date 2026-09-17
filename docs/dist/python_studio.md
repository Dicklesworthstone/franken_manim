# Python Studio preview

Run an existing Python Scene in a disposable process and inspect its native
frames in the same authenticated Studio UI used by native scene workers:

```bash
fmn-python studio lesson.py Example
fmn-python --robot studio lesson.py Example --resolution 960x540 --fps 24
```

Or keep the preview alongside other Python work:

```python
from fmn_python.studio import Studio

with Studio("lesson.py", "Example", resolution=(640, 360), fps=30) as preview:
    print(preview.url)
    input("Press Enter to close Studio")
```

The optional installed Python wheel owns the interpreter. The standalone `fmn`
executable has not acquired a CPython dependency, locator, or Python launcher.
The worker uses the exact host interpreter/virtual environment and installed
engine, not a `python` found later on PATH. Install the wheel before launching;
source-tree `PYTHONPATH` overlays are deliberately not propagated to a worker.

## Capture, playback and reload

The worker loads the selected source, constructs the requested class normally,
and runs its ordinary lifecycle once. `play`, `wait`, native/Python updaters,
and camera synchronization use the existing scene scheduler. Lumen's retained
camera renderer supplies the actual PNGs. Matching native inspector snapshots
are captured at those same frame boundaries. Static scenes receive one final
capture, rather than a blank or lifecycle-only “preview.”

The UI becomes available after this initial capture. Its playback and scrub
controls read the captured timeline, including reverse scrubbing, without
executing the source or its side effects again. The inspector's mobject records
and hierarchy follow the displayed frame. The viewport is read-only; no live
Python input-event editing or projected-camera picking is advertised.

**Reload executes source again**, in a new worker with fresh helper imports,
and starts the new timeline at frame zero. Equal-size, same-timestamp source
edits do not reuse old Python bytecode. Starting at zero also permits a shorter
edited scene to replace a longer one. A syntax error in the entry source is
reported before replacing the current worker. A runtime failure is contained
in the disposable worker; the last displayed PNG and stable host remain, and
fixing the source permits another explicit Reload. A failed capture is not
published as a successful truncated timeline, even if scene code catches its
exception. Automatic crash re-execution is disabled.

Committed preview positions are explicitly opaque journal barriers, not a
serialization of arbitrary Python callbacks. This implementation does not
restore callback checkpoints, replay arbitrary Python effects, or export a
durable certified Python session. It also does not provide IPython `embed`,
audio playback, or the full live `InteractiveScene` editing lifecycle.

## Budgets and host access

The defaults are 640×360, 30 FPS, 7,200 captured frames, a 256 MiB encoded capture
budget, and a 120-second worker-request timeout. Set `--max_frames`,
`--max_bytes`, or `--timeout` explicitly for longer scenes. The encoded budget
accounts for PNGs, inspector documents and per-frame metadata; native inspector
traversal/field limits and a 16M-pixel frame ceiling apply independently. It is
not an operating-system memory quota on arbitrary authored Python. The maximum
accepted controls are 100,000 frames, 1 GiB encoded captures, and 900 seconds.

The native HTTP host binds only loopback and requires its random bearer
capability for the UI, frames, inspector and mutation routes. Its existing
origin, request-size, concurrency, and rate limits remain in force. The URL is
a capability: do not share it with untrusted users. There is no automatic
browser opening or public-network bind. Ctrl-C or `Studio.close()` stops the
host and reaps its disposable worker; context-manager use is recommended.

Preview is silent and needs no ffmpeg. Use `render_scene` / ordinary
`fmn-python lesson.py Example` for soundtrack or movie export.

**Process isolation is not a sandbox.** Scene code has the host user's file and
process permissions. Source/runtime digests bind this preview generation; they
do not constitute the complete C1–C10 certified Python input closure. Python
Studio does not make a certified-render claim.
