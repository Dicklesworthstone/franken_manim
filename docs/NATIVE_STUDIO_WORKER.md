# Executable native Studio workers

`fmn_studio::native` connects an owned native Scene to the existing isolated
worker protocol, retained CPU renderer, interactive editor and supervisor
recovery path. It does not serialize native closures or pretend that changing
a clock label executes them.

## Run the browser host

From a checkout with the repository's pinned Rust toolchain:

```sh
cargo run --locked -p fmn-studio --example native_live
```

Open the private loopback URL printed by the program. The embedded Studio UI
can scrub the rotation/updater scene, send native editing input, inspect the
live arena, request debug overlays and restart the worker. Press Enter in the
launching terminal to stop the host and reap its child. The example uses
`StdHostEntropy`, whose audited implementation currently requires Unix; other
hosts must provide an audited entropy capability rather than a predictable
fallback token.

The parent runs the existing `Supervisor`, `StdWorkerLauncher`, `FrameHub` and
`StudioHost`. Only the disposable child constructs or executes the scene.
The child is launched from the exact absolute executable with an explicit,
cleared environment and the ordinary bounded, versioned pipe protocol.
The example checks the executable digest at handshake and uses a bounded
process-lifetime virtual checkpoint cache; it does not leave cache files.
Its Restart action relaunches the same already-built executable. It does not
invoke a compiler or implement incremental source rebuilding.

For a headless application-level acceptance run:

```sh
cargo run --locked -p fmn-studio --example native_live -- --self-test
```

This invokes an actual native callback panic in a child process, requires the
supervisor to launch a new worker generation, and compares complete rendered
frame responses before/after recovery. It also drives authenticated loopback
HTTP requests for UI serving, committed scrub, editing, undo, inspection,
overlays and restart. It uses the real renderer and real socket/pipe paths,
not a replacement worker or synthetic PNG.

## Build a native worker

Construct a fresh `Scene` and owned `Vec<NativeSegment>` in a factory.
`NativeSegment::Play` owns native animations plus `PlayOverrides`;
`NativeSegment::Wait` uses the Scene's ordinary wait duration semantics.
`NativeSceneProgram::from_interactive` also accepts an existing native editor
without registering another set of interaction listeners.

`NativeSceneProgram` requires frame zero, play count zero, and no skip/range
or presenter mode. It steps through Proscenium's existing begin/prepare/complete/
finish operations. Segment finalization occurs when execution moves beyond
its last capture, not prematurely while paused on that capture. This preserves
updater suspension/resumption, animation cleanup and ordinary capture order.
The capture packet is checked against the actual rational runtime clock.
Empty plays and zero-duration waits follow the engine lifecycle.

Index zero is the constructed state. Indices one and later are actual Scene
capture indices. Set the program's `frame_limit` to the highest admitted
capture and the worker's `frame_count` to that limit plus one. Decimal floating
point durations are quantized by the existing rational-clock rules; do not
assume that `0.1` seconds always means exactly three samples at 30 fps.

Construct `NativeWorkerConfig` with the scene name, actual executable digest,
source/input closure digest, frame count, fps and retained renderer policy,
then pass it and the factory to `NativeSceneWorker::new`. Pass that worker to
`serve_worker` inside the disposable child. Use the same `ProtocolLimits` for
the service and pipe driver. The example is a complete composition template.

The worker keeps one live cursor, not a prerendered movie. Clean forward scrubs
continue the same native callback owner. Backward scrubs rebuild from the
factory and execute to the requested capture. The implementation retains at
most the current capture packet during stepping; the native Scene and its
animation/editor snapshots still have their ordinary memory costs.

## Camera scenes: surfaces, images and dot clouds

The default `camera: None` retains the affine vector renderer. Select a fixed
camera explicitly to render a mixed native scene through Lumen's existing
`RetainedFrameRenderer::render_with_camera`:

```rust
worker_config.renderer.engine = fmn_render::EngineIdentity::certified();
let viewport = worker_config.renderer.frame.viewport;
worker_config.camera = Some(fmn_render::CameraConfig {
    resolution: (viewport.width, viewport.height),
    fps: worker_config.fps,
    background: worker_config.renderer.frame.background,
    ..fmn_render::CameraConfig::default()
});
```

The camera route accepts vectors, UV-grid surfaces, triangle meshes, image
quads and dot clouds in their shared painter sequence. Projection, clipping,
depth testing, lighting, texture sampling, fixed-in-frame content and glow all
remain Lumen responsibilities. Scene animations and updaters run normally;
the camera configuration itself is fixed for the worker session.

The viewport, background and FPS must agree with the worker configuration.
Fast-CPU and annex identities are refused because the selected mixed camera
rasterizer is the certified CPU implementation. This identifies the executed
engine; it does not claim that this worker has passed a cross-platform release
certification matrix.

Camera frame provenance records the normalized projection, light, sample
ceiling, background, output geometry, tile policy and renderer version. The
same digest is carried as the `native/camera-capture-policy` journal read.
A changed camera or light therefore invalidates journal replay even when
geometry and source identity are unchanged. Scheduler thread counts are not
semantic identity. Use the supervisor's normal read-validation path when
restoring checkpoints: static camera policy is journal metadata, not a mutable
SceneState field.

Camera inspection includes the timeline and native object records, but reports
`input_events: false`. Perspective input projection and projected debug
overlays are not yet implemented; their requests refuse before mutating the
live owner instead of applying the affine editor to perspective pixels.

Run the complete camera browser host, showing a rotating lit surface, a
fixed-frame raster image and a glow dot:

```sh
cargo run --locked -p fmn-studio --example native_camera
cargo run --locked -p fmn-studio --example native_camera -- --self-test
```

The self-test uses the actual child executable and authenticated HTTP server.
It checks animated PNG differences, inspection capabilities and bit-identical
camera frames after supervisor restart. Neither native example needs CPython,
system fonts or ffmpeg.

## Input, journals and recovery

On the affine route, `Event` dispatches through the existing native
editor/application listeners at the paused frame, then renders a fresh PNG.
Inspection and overlays read that same mutated arena. Events do not advance
the clock or run scene updaters. These edits are transient: the next seek
discards them by reconstructing from source. They are not falsely reported as
durable replay-journal edits.

`Play` accepts the canonical `studio_seek_command` identity. It executes from
a fresh factory and records actual `Scene::state_bytes()`, including the real
clock, RNG and play count. The entry carries the source/input closure, worker
build and capture contract, plus the camera policy when selected. Checkpoints
are attached on the configured frame-distance cadence. No made-up RNG fork
or journal-position-as-play-count is substituted for runtime state.

Replay is disabled by default. Unclassified factory commands are opaque
barriers, and restore/replay requests refuse. Selecting
`NativeReplayPolicy::ColdVerified` is an explicit author attestation that each
factory call creates independent animation/updater/listener captures and that
execution depends only on the content-hashed closure. No unjournaled external
I/O or shared mutable factory captures may influence it. Even this policy is
reported as stateful, not frame-parallel pure.

A cold-verified restore decodes checkpoint metadata only to identify its
frame. The decoded callback-free arena is never installed. A new factory
executes the actual program from zero; its complete state bytes must match the
checkpoint before the new live owner is installed. A following forward scrub
continues the reconstructed mutable callback state rather than starting over.

Journal replay validates ranges, source/effect identities, renderer records,
event ownership, checkpoint digests and the aggregate frame-work budget before
invoking factories. Each requested committed state is freshly executed and
hash-checked. A divergent later entry installs no earlier prefix. Only verified
executed entries can replace the crash tail. Empty replay preserves the
existing live owner and tail. A failed live input/forward execution invalidates
that cursor rather than using record restoration as fake closure rollback;
a successful seek can reconstruct a healthy cursor.

Wire responses are checked as complete encoded envelopes before committed
journal state is published. Budgets also cover source frame bounds, total
replay steps, viewport pixels, checkpoint bytes and inspector/overlay JSON.
The supervisor's existing process deadlines remain the boundary for callbacks
that do not return; an in-process frame counter cannot interrupt arbitrary
native code.

## Scope

This is an explicit native factory front door; the default `fmn-cli` captured-
artifact worker is unchanged. The runnable examples are not a claim that the
CLI can compile and discover arbitrary Rust source files. Native programs use
an owned declarative segment schedule, not a suspended arbitrary Rust stack.
The renderer supports affine CPU vectors and explicitly selected fixed-camera
mixed CPU captures. It does not add animated cameras, camera-space editing,
projected overlays, Metal presentation, Python callback ownership or audio
export to this worker.

Recovery is cold execution, not constant-time checkpoint restoration. It has
not been benchmarked against the Studio latency budget. Source-hash checking
and state equality detect divergence but do not sandbox side effects; the
factory contract and disposable process boundary remain essential. Native
editing history does not rewind mutable closure captures or external effects.

## Validation

At implementation checkpoint `04990eb`, the focused GitHub Actions lane
compiled and passed all 159 `fmn-studio` tests plus all 16 native history tests,
and the real-process/HTTP host acceptance completed successfully. These results
belong to that exact checkpoint, not to later camera additions.

The workflow runs both real-process/HTTP example self-tests. Native animation
tests compare serialized captures with ordinary Scene playback. The
`native_camera` integration suite compares all five primitive kinds and mixed
scenes with direct Lumen output; tests motion/rewind/recovery, 1/4/16-thread
frame identity, changed camera-policy replay refusals and capability reporting.

The focused lane supplements, and does not replace or relax, the repository's
mandatory workspace governance, formatting, lint and certification gates.
A focused execution pass is not a claim that the full workspace gate is green.
