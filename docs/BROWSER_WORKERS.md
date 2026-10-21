# Worker-owned browser recordings

`WorkerTimelineSession` (`demo/wasm/worker-session.mjs`) admits one FMTL recording
through a dedicated picture worker. The shipping entry point imports the fixed
local `pkg/fmn_wasm.js` module and uses `FmnPlayer.from_bundle_with_audio`,
`take_soundtrack`, `render_into` and `seek_frame`. It never executes scene code or
fetches source media. The native decoder's identity, checksum, camera, snapshot
and resource checks remain authoritative. No JavaScript FMTL decoder is added.

Decode and every raster, including frame zero, run outside the UI thread. The
main thread owns controls, canvas presentation and the existing Web Audio clock.
This keeps a slow picture from blocking audio-window refill or a Pause click.
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

Sound is extracted once in the picture worker and copied in native bridge windows
of at most 65,536 sample frames into transferable Float32 channel buffers. The
worker frees its extracted native PCM and transfers those buffers without a
structured-clone copy. The main thread adapts that ownership to the existing
`prepareSoundtrack` scheduler. Small audio uses its eager path; larger audio
retains one transferred PCM owner and three bounded device windows. There is no
second native player or retained picture arena on the UI thread. Conversion can
transiently hold native PCM and transferred PCM together, and the Wasm allocator
may retain freed capacity. The old movie and replacement candidate also overlap;
none of these limits is a process-RSS guarantee.

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
position. The existing audio owner is reused: the replacement decodes and frees
its native soundtrack without copying or transferring another set of channel
buffers. No new audio device is created and restart never starts playback.
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
