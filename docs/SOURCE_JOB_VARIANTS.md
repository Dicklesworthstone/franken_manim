# Process-isolated scene variants

`render_source_scenes` accepts `SourceRenderJob` entries as well as its existing
class-name strings. One reusable scene can produce a parameter sweep, translated
lesson variants, or different seeds without declaring another subclass for every
output. Each job executes in a fresh interpreter through the existing scene,
render-session, process-supervision and publication owners.

```python
from fmn_python import render_source_scenes
from fmn_python.source_jobs import SourceRenderJob

if __name__ == "__main__":
    result = render_source_scenes(
        "lessons.py", "recordings",
        [SourceRenderJob("seed7", "Lesson", {"random_seed": 7}),
         SourceRenderJob("seed11", "Lesson", {"random_seed": 11}),
         "ClosingScene"],
        format="fmtl", workers=2, bundle_camera=True, bundle_audio=True,
        checkpoint="recordings-progress.json", resume_key="lessons-assets-v1",
        continue_on_error=True,
    )
    print(result.as_dict())
```

`name` is the output identifier and receipt name. `scene` is an exact, locally
declared class identifier in the source loader's inventory, not an attribute
path or expression. Repeated classes are allowed; output names must be unique
under the existing filesystem-normalization rules. Strings still select a class
and output with the same name. Mixed lists preserve input order in reports.

Constructor values must be plain JSON: dictionaries with string keys, lists,
strings, finite numbers, booleans and null. Tuples, custom subclasses, native
objects, functions, cycles and sets are refused, not converted into a different
constructor invocation. Values are copied before workers or observers run, so
mutating caller dictionaries or a previous worker's nested values cannot alter a
later job. There are limits of 1 MiB encoded kwargs per job, 8 MiB in aggregate,
65,536 visited constructor nodes and 64 nested levels; integers are capped at
4,096 bits. The existing `max_jobs` ceiling separately bounds job admission.

The same standard-mode output modes supported by process batches remain
available, including FMTL camera/audio export. Format restrictions still apply:
for example FMTL cannot combine with raster threads, partial capture or paired
output. Explicit `SourceRenderJob` entries do not support `reproducible=True`:
constructor-input certification is not implied by a typed JSON plan. Existing
string-only reproducible calls retain their prior semantics.

## Recovery

Add `resume=True` to the same invocation to hash-verify completed artifacts and
retry only unfinished variants. Both the source-class identifier and the exact
JSON constructor values are bound into each checkpoint plan. `true`, `1` and
`1.0` are distinct, as are positive and negative floating zero. Key order is not
significant. Worker count and timeout can change; class selection, arguments,
recording settings, entry source, working directory and output names cannot.

Existing string-only plans remain byte-compatible. A parameter-variant plan uses
`source-variants-v1` and cannot reinterpret a legacy `source-file-v1` checkpoint,
even when one explicit variant happens to select its identically named class
with no kwargs. Helpers and assets still use the caller-owned `resume_key`
contract; a checkpoint is not a certified source closure or a cache.

Publication is still create-only and per artifact. A crash can leave a published
file without a receipt; recovery does not overwrite or silently adopt it.
Timeouts and failures preserve completed receipts; observers run after durable
checkpoint publication. No new scheduler, recorder, mixer or serializer is used.

## Validation

`test_source_job_variants_protocol.py` exercises the production process batch,
worker dispatch, JSON receipt channel and locked checkpoint journal using real
spawned interpreters. Its explicitly reduced source/compiler/render boundary
writes NOT-FMTL bytes; these tests do not establish native image/audio acceptance.
The installed-wheel test separately compares native variants with sequential
`RenderJob` exports. Missing native tooling is an error with `FMN_TEST_NATIVE=1`.

## JSON job files and the CLI

A job file keeps constructor data separate from Python source:

```json
{
  "schema": "fmn-python.source-jobs",
  "version": 1,
  "jobs": [
    {"name": "seed7", "scene": "Lesson", "scene_kwargs": {"random_seed": 7}},
    {"name": "seed11", "scene": "Lesson", "scene_kwargs": {"random_seed": 11}}
  ]
}
```

```sh
fmn-python lessons.py --format fmtl --workers 4 --jobs-file variants.json \
  --bundle-camera --bundle-audio --video_dir recordings \
  --checkpoint recordings-progress.json --resume-key lessons-assets-v1 --keep-going
```

Append `--resume` to reuse hash-verified completed variants. The CLI reads and
validates the job file before isolated source discovery, checks every selected
class against discovery, then uses the same process API. One-job files are still
isolated batches. Receipts and filenames use job names, while lookup uses class
names. Common capture/audio options apply to every job. Omitting this flag leaves
ordinary single-scene, named-scene and write-all behavior unchanged.

`--jobs-file` requires `--workers`, and cannot combine with positional scene names,
`--write_all`, `--output` or `--file_name`. The same job files work with standard PNG stills/sequences, GIF, Y4M, WAV,
SVG, MP4, MOV and FMTL. Omitting `--format` keeps the existing PNG-sequence
default. `load_source_jobs(path)` from `fmn_python.source_job_file` also returns
a tuple accepted by `render_source_scenes`. No new render loop is involved.

Files must be stable regular UTF-8 files of at most 8 MiB. Symlinks, special files,
unknown fields/versions, duplicate JSON keys, invalid values, colliding names and
excessive job counts are refused before scene discovery. `scene_kwargs` is
optional and defaults to an empty object. File whitespace, key order and the file
path itself do not affect checkpoint identity; the admitted ordered job values
do. A file changed after admission cannot change jobs already snapshotted.

Malformed or unreadable job documents use CLI exit 2 and retain robot error
reporting. Other existing batch exit/partial-receipt behavior is unchanged.
`test_source_job_file_protocol.py` covers the shipping wrapper and process API;
its parser/inventory/native-capture boundaries remain explicit fixtures. The
native test invokes the installed CLI independently and must be run against a
matching built wheel; protocol success does not establish that acceptance.


## Parameterized media output

The ordinary media renderer accepts the same JSON job document, without an
intermediate FMTL export or another scene execution:

```sh
fmn-python lessons.py --workers 4 --jobs-file variants.json --format mp4 \
  --crf 18 --preset slow --threads 2 --video_dir videos \
  --checkpoint video-progress.json --resume-key lessons-assets-v1 --keep-going
```

This writes `videos/seed7.mp4` and `videos/seed11.mp4`, using independent
instances of `Lesson`. Each worker receives the original typed JSON constructor
values. `--workers` controls scene concurrency; `--threads` controls each native
renderer and defaults to one on the process path. MP4/MOV still require the
existing governed ffmpeg capability. Audio in media outputs follows the ordinary
scene/media pipeline; `--bundle-audio` and `--bundle-camera` remain FMTL-only.

Use `-n START,END` or `-n START` for a segment excerpt, `-s` for a final PNG, or
`--save-last-frame` for a companion PNG from the same scene execution. Subdivided
media output also accepts job files:

```sh
fmn-python lessons.py --workers 2 --jobs-file variants.json --format y4m \
  -n 2,5 --subdivide --save-last-frame --video_dir excerpts
```

Clips use each job's output name, not its class name. These options delegate to
the existing playback, paired-output and subdivision owners and retain their
format restrictions. Non-subdivided single outputs and pairs support verified
checkpoint recovery. Subdivided collections still refuse checkpoints. Explicit
constructor variants refuse `--reproducible` before source discovery; this does
not change certification for ordinary class-name batches.

All job-file classes must be present in isolated discovery before any render
worker starts. Output collisions stop admission without overwriting files.
Job data is read once before discovery, so subsequent edits to the document do
not alter admitted work. Robot output retains the ordered batch report and
partial outcomes on failure/cancellation; `--quiet` does not suppress it.
Malformed documents or conflicting selection options fail before discovery.
Help describes the format without opening the job file.

`test_media_job_file_protocol.py` runs the full bundle/media dispatch wrappers,
production job reader, range/video option consumers, worker dispatch, real
spawned interpreters, receipt transport and filesystem checkpoint owner. Native
common-option parsing, capture/rendering, discovery inventory and paired/clip
owners are explicit fixtures; emitted NOT-MEDIA files are not image/audio
acceptance evidence. `test_media_job_file_native.py` separately exercises the
installed module entry point and native outputs, with `FMN_TEST_NATIVE=1`
requiring a matching native wheel rather than silently skipping acceptance.
