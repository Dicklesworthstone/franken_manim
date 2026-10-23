# Worker-owned browser recordings

`WorkerTimelineSession` (`demo/wasm/worker-session.mjs`) admits one FMTL recording
through a dedicated picture worker. The shipping entry point imports the fixed
local `pkg/fmn_wasm.js` module and uses `FmnPlayer.from_bundle_with_audio`,
`take_soundtrack`, `render_into` and `seek_frame`. It never executes scene code or
fetches source media. The native decoder's identity, checksum, camera, snapshot
and resource checks remain authoritative. No JavaScript FMTL decoder is added.

Decode and every raster, including frame zero, run outside the UI thread. The
main thread owns controls, canvas presentation and the existing Web Audio clock.
This keeps a slow picture from blocking a Pause click or the browser UI.
A worker-owned PCM request waits for an already executing raster; if the audio
lookahead runs out, the clock stops at the first unheard sample until refill.
It does not make the renderer faster or claim that every preview picture is shown.

## Ownership and bounds

There is one active raster request and one latest pending frame index, not a
queue of promises or frame buffers. Rapid scrubbing replaces the pending index.
During playback, completed previews may be shown late; an explicit seek or pause
invalidates older presentation requests. Neither behavior changes the recorded
frame grid or its native reconstruction. One RGBA8 ArrayBuffer moves between the
worker and the browser; the canvas callback borrows its view synchronously. Copy
it explicitly to retain pixels after the callback returns. Wasm-bindgen still
marshals `render_into` through Wasm storage: this is not zero-copy Wasm rendering.

A new load gets a separate candidate worker. Native admission and frame zero
must succeed, and the audio adapter must accept any soundtrack, before the old
movie is released. A failed candidate leaves the current movie usable. Starting
another load aborts its predecessor's read signal and terminates its candidate;
disposal also terminates outstanding decode/raster work. At most the current
worker and one candidate worker are owned by a session. A 30-second request
watchdog terminates a nonresponsive worker; API consumers may set `timeoutMs`.

The input and decoded-PCM limits are each 256 MiB. Each viewport dimension is in
1..4096. Metadata allows 4096 labels, each at most 4096 code units, with a combined
1 MiB UTF-8 label budget. Exceeding these limits is a named refusal, not truncation.

The shipping page enables `streamAudio: true`. The picture worker extracts and
retains its native soundtrack once. It transfers no full-length channel arrays.
Instead, the existing audio scheduler requests random-access PCM windows through
`read_window(start, count, signal)`. Each request and response is independently
range-checked, correlated, and limited to 65,536 frames per channel (512 KiB for
stereo float32). One live audio request may wait alongside the single latest
preview index; audio receives the next worker slot before pending previews. An
already running raster is not preempted or duplicated.

The same Web Audio scheduler handles synchronous native PCM and asynchronous
worker windows. It retains at most three bounded device buffers and one pending
transfer, not a second recording-sized PCM array. Initial admission awaits one
verified window without starting sound. Refill callbacks only prepare a window;
`frameAt()` owns subsequent scheduling. Delayed reads freeze at the first
unqueued sample and resume from that phase, preserving fractional seeks, device
resampling halos, audio tails, and the silent picture suffix. Pause, seek and
disposal cancel pending reads and invalidate late completions. A transfer or
device failure stops playback rather than retrying indefinitely.

`prepareSoundtrack` returns a promise for asynchronous PCM sources; ordinary
native sources retain their synchronous API. `WorkerTimelineSession` awaits
both kinds of adapters transactionally. The API defaults to `streamAudio: false`
for existing custom adapters: that legacy route still transfers full channel
arrays and uses eager/retained playback according to their size. Set the option
explicitly to use bounded transport, as in the example below. An old worker that
does not acknowledge windowed mode is refused, never silently used eagerly.

The 256 MiB decoded-PCM admission limit is unchanged. Native decoding still
retains the full soundtrack for random access, and Wasm may retain allocator
capacity. The old and replacement workers overlap during transactional load or
restart. Device buffers, transferred windows, the input envelope, canvas storage
and browser-internal allocations are additional; this is not a process-RSS cap.

A failed current-worker request pauses its associated audio and reports the
failure. A wrong response identity, frame, viewport or buffer shape is refused
before presentation. There is no silent fallback to UI-thread rasterization.
Playback remains presentation, not an encoded or certified artifact.

## The shipped player page

`demo/wasm/player.html` mounts `player-ui.mjs` and uses this worker session by
default for both silent and narrated recordings. The UI no longer imports or
instantiates the native Wasm module. Audio still advances on its device clock;
late raster responses update only the preview. During an explicit seek, Play is
held until the selected picture arrives. Pause invalidates outstanding display
work, and the final requested picture may complete after the soundtrack stops.
Labels, camera metadata, local-file loading and failed-replacement recovery are
preserved. The synchronous `TimelineSession` API remains available independently.

Local files and the demo response are read through `bundle-input.mjs`. Known
lengths allocate one exact bounded byte buffer; unknown lengths retain bounded
chunks then assemble one input buffer. A superseding load aborts and cancels the
old reader. Actual bytes, declared size, chunk count and EOF are checked before
worker admission, including responses without Content-Length. The file picker
never uploads recording bytes.

## API

```js
import {WorkerTimelineSession} from './worker-session.mjs';
import {prepareSoundtrack} from './audio-playback.mjs';
const session = new WorkerTimelineSession({
  width: 640,
  height: 360,
  streamAudio: true,
  prepareAudio: player => prepareSoundtrack(player, () => new AudioContext()),
  onFrame: ({rgba, width, height}) => context.putImageData(new ImageData(
    new Uint8ClampedArray(rgba.buffer, rgba.byteOffset, rgba.byteLength),
    width, height), 0, 0),
  onError: error => showError(error.message),
});
await session.load(async signal => readRecording(signal));
session.present();
session.render(10);
// For an explicit new seek rather than a forward preview update:
session.invalidate();
session.render(3);
// session.current.audio uses the existing play/pause/seek/frameAt contract.
// Only call audio.play() from a user gesture. Dispose when the owner goes away.
session.dispose();
```

The read callback returns a Uint8Array and receives an AbortSignal. Its returned
buffer is transferred and detached; return fresh storage for another load. A
subarray is copied first so unrelated bytes are not transferred. Native and
custom adapters must not retain the transferred input view.

## Executable evidence

`node --test demo/wasm/worker-session.test.mjs` exercises the production protocol,
worker runtime, main-thread ownership and existing audio scheduler with explicit
native-API/device fixtures. It uses real structured cloning and transfer-list
detachment, including cancellation, failure and rapid-seek races.

`python scripts/check_picture_worker_browser.py` uses real Chromium workers,
transferable buffers and Web Audio preparation, with an explicit deterministic
native-API fixture. It checks exact PCM transfer and UI heartbeats during slow
worker work. It uses in-memory Blob modules, with a classic-worker import
bootstrap because Chromium disallows module-worker startup from an opaque-origin
page. It does not validate hosted module-worker startup, native Wasm decoding,
physical audio hardware or other browsers. The shipping entry point is a module
worker served alongside the generated Wasm package.

`python scripts/check_worker_player_page.py` mounts the production UI in the
actual page markup and drives its file picker, labels, Play/Pause and scrubber
through Chromium. It uses the same explicit decoder fixture and offline module
bootstrap described above. Real Web Audio verifies gesture-controlled playback,
phase-preserving pause/resume, seek offsets, the silent suffix, replacement and
device cleanup. It does not substitute for native decoder/render acceptance.
The existing `scripts/check_browser_soundtrack.mjs` keeps its native PCM and
pixel assertions and now serves the worker modules and initializes its separate
main-thread native oracle explicitly.


## Restart a failed picture worker without reopening the recording

`await session.restart()` starts one replacement worker, rereads the current
source through its original reader, and reconstructs the paused picture before
publishing the replacement. Narration stays paused at its exact fractional
position. The existing audio owner and device are reused. In windowed mode,
the validated replacement retains its decoded PCM and the stable source adapter
is rebound only when replacement succeeds. No full channel arrays are copied.
In legacy eager mode the replacement frees its decoded soundtrack as before. No new audio device is created and restart never starts playback.
For silent recordings, the default restore position is the last delivered
picture; `restart(frame)` may name an explicit recorded frame.

The worker protocol is `fmn.picture-worker.v2`. The loaded source identity binds
its byte length and the canonical envelope's trailing SHA-256 checksum over all
preceding header/payload bytes. Both initial load and restart must pass the
native FMTL decoder. Reading a trailer is **not verification**: altered bytes
with an unchanged trailer still reach and must pass native checksum validation.
A different validated checksum, byte length, engine, clock, camera metadata,
labels or audio layout refuses replacement rather than pairing new pictures
with retained narration. This is content identity, not publisher authentication.

Readers supplied to `load` must produce a fresh `Uint8Array` each time to support
restart; the original buffer was transferred. A `File` reader naturally retains
its immutable local source, and a URL reader fetches and validates the current
bytes again. The session retains the reader, not an extra owned recording-sized
copy. Storage retained by a caller's closure is still the caller's storage.
Failed candidate reads, decoding, rendering or metadata checks preserve the
previous session and its soundtrack. A failed old worker stays failed until a
replacement succeeds; a healthy old worker stays usable if replacement fails.

There is at most one restart candidate. A repeated restart during recovery
returns false, a new load supersedes it, and `cancelRestart()` aborts its read
signal and terminates any candidate. Late results cannot replace the current
movie. Disposal releases both workers and the single audio owner. Restart has
no automatic retry loop: deterministic rendering failures remain visible.

The player page exposes **Restart picture worker** even when a worker failure
has disabled playback and scrubbing. During recovery the same button becomes
**Cancel restart**; other playback controls wait for the replacement picture.
Success returns to a paused, usable recording without requiring the file picker
again. Hiding the page, stopping, loading another recording or disposing the page
cancels recovery. A late success/failure cannot change a newer load's controls or
status. A persistent failure at the requested picture remains a reported failure,
not a hidden frame substitution or automatic retry loop.

`node --test demo/wasm/player-recovery.test.mjs` exercises restart/cancel
control ownership and late-result races with explicit DOM/session fixtures. The
Chromium page check additionally clicks the real controls after a worker protocol
failure, verifies the same paused audio owner and device, cancels recovery, and
checks changed-source refusal and retry. These checks retain the native-decoder
fixture boundary described above; they do not establish native Wasm acceptance.

## Bounded audio validation

`node --test demo/wasm/worker-audio.test.mjs demo/wasm/async-audio.test.mjs`
exercises window budgets, exact sample order across delayed refills, cancelled
requests, hostile responses, fractional positions, async admission and worker
restarts using explicit native/device fixtures and real buffer transfers.

`python scripts/check_streamed_audio_browser.py` compares the asynchronous
scheduler's real Chromium `OfflineAudioContext` output against a full-buffer
reference at nine source/device-rate and seek combinations, with the existing
1e-5 absolute tolerance unchanged. It also exercises a real worker and real Web
Audio over a long synthetic PCM source, including recovery without a new device.
By default this uses an ephemeral loopback server and ordinary module workers.
`--in-memory` runs when browser policy refuses loopback navigation; it imports
Blob modules through a classic-worker bootstrap and explicitly reports
`normalModuleWorkers: false`. That mode does not prove hosted module-worker
startup. Both modes use a named native-decoder fixture, not generated Wasm,
physical audio hardware, cross-browser evidence or a native acceptance gate.

## Live viewport changes and responsive previews

`session.resize(width, height)` changes the existing worker's preview viewport
through native `FmnPlayer.set_viewport` and `render_into`. It does not reread the
FMTL source, decode another player, transfer the soundtrack again, seek audio,
or create another audio device. The selected frame is rerasterized at the new
resolution. Recorded camera tracks, frame sampling, and bundle bytes are not
edited; projection and aspect-ratio fitting remain the native player's policy.

Each dimension must be an integer in 1..4096, validated before changing the
requested viewport. The method returns whether the requested dimensions changed;
completion arrives through `onFrame`, not through that boolean. Consumers must
size their canvas backing store to the callback's width/height before presenting
its pixels. The old displayed picture remains available until a complete new
picture arrives. A failed raster follows the existing visible worker-failure and
restart path, rather than presenting partially resized pixels.

Resize and seek storms share one latest pending frame/viewport pair behind the
single active request. A superseded image cannot repaint the page at an obsolete
size. Returned buffers are reused where their byte length permits, or replaced
in the worker. Audio keeps priority for the next available request slot; an
already executing raster is not preempted. A native resize may transiently hold
the returned old RGBA buffer plus its newly allocated replacement, each bounded
by the existing viewport limit (64 MiB at 4096 by 4096). Native renderer storage,
Wasm allocator capacity, canvas backing storage and browser allocations are
additional: this is not a total-memory or latency guarantee.

A viewport can also be selected before loading. Changes during admission or
restart reach the candidate, and later loads/restarts keep the selected size.
`invalidate()` still cancels outstanding presentation intents; call `render()`
to request a picture again. The page does that after Pause when a viewport change
is pending, without resuming or seeking narration.

The shipping page's **Preview resolution** control offers automatic high-DPI
16:9 preview, 640x360, 1280x720, 1920x1080, 1080x1920 portrait, and 1080x1080
square. Automatic mode follows the canvas's CSS width and device pixel ratio,
rounds to a 16:9 pixel grid, and caps at 1920x1080. Hidden zero-width containers
wait for a positive measurement. ResizeObserver, window changes and pixel-density
changes coalesce to one measurement per animation frame, and all observers and
queued callbacks are released on disposal. Explicit presets ignore CSS/density
changes. Higher resolutions cost more raster work; the 640x360 preset retains
the earlier fast preview size. These are preview controls, not new export modes.

`worker-viewport.test.mjs` checks native-adapter dispatch, buffer ownership,
coalescing, stale response rejection, audio priority, and load/restart races.
`player-viewport.test.mjs` covers the shipping controller, sizing policy and
observer cleanup. `python scripts/check_player_viewport_browser.py` drives the
actual page in Chromium, including high-DPI mobile resizing, landscape/portrait
pixels, running/paused Web Audio and worker recovery. It uses the explicit
native-decoder/raster fixture and offline Blob-module bootstrap described above;
it does not establish native Wasm, hosted module-worker, other-browser or
physical-device acceptance.
