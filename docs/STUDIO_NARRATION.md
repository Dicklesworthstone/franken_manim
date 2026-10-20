# Recorded narration in Studio

A recorded-mode Studio host can expose a validated FMTL soundtrack alongside
its preview using `StudioRecordedAudio::decode` and
`StudioHost::serve_recorded_until`. The embedded page now offers **Play narration**,
**Pause narration**, and volume. A normal live host without the recorded-audio
transport reports that absence; it does not substitute source media or silence
for missing narration. These controls do not turn arbitrary live scene programs
into an audio producer.

Playback starts only after a user click and a synchronized picture. The picture
clock and worker generation must match the audio metadata before the first
sample is scheduled. A temporarily unavailable worker can be retried with the
same button; initial metadata probing never acquires an audio device.

## Clock and memory behavior

Narration follows Web Audio's device clock. At most ten preview requests per
second select original recorded picture frames through the existing Studio
seek queue. A slow renderer may show late or skipped *preview* pictures while
audio continues; this is not a new rendering grid or a frame-accurate video
export. The final picture is held through a longer audio tail. A shorter
soundtrack leaves silence through the remaining picture interval.

The browser fetches authenticated, checksummed interleaved Float32 windows.
Every window verifies its complete-recording hash, worker generation, start,
length, next cursor and EOF flag before scheduling. Responses are streamed
through a hard byte ceiling even without Content-Length. No response URL
contains the capability token: audio requests use the existing capability
header. Redirects, mismatched media types, nonfinite PCM, truncated data and
stale generations stop playback rather than being played optimistically.

Each request contains at most half a second of samples, capped at the server's
65,536-frame limit. At most three AudioBuffer sources are queued, plus one
in-flight bounded fetch; the full recording is never downloaded into a browser
AudioBuffer. Adjacent nodes use independently computed absolute sample times,
not network arrival times or sums of rounded frame durations.

Pause keeps the exact audio sample cursor, not the most recently displayed
picture index. Seeking starts from the selected picture's absolute rational
sample boundary. Editing, live playback, worker replacement, picture errors,
connection loss, page hiding or leaving the window stop queued sound and abort
pending fetches. Delayed replies from an earlier play or seek cannot restart
cancelled sound.

The embedded page recovers automatically from a delayed chunk or browser tick.
An underrun enters **buffering** at the first unplayed sample and freezes the
picture cursor there. It retains the single pending fetch rather than aborting
and requesting the same window again. When that verified window arrives, the
player rechecks picture-clock/worker ownership and resumes from the same sample
on a fresh device-clock anchor. Repeated stalls cannot silently skip or repeat
samples. A fully queued short soundtrack still admits its intentional silent
picture suffix, and a longer soundtrack still holds the final picture.

**Pause narration** remains available during buffering and cancels the pending
recovery. Device suspension, a lost connection, stale ownership, HTTP refusal,
checksum failure, or malformed PCM stops playback; these failures are not retried
in a loop. Automatic recovery neither resumes a suspended device nor increases
the three-buffer/one-fetch limit. Hosts using the controller directly retain
manual-underrun behavior unless they explicitly select `autoRebuffer: true`;
the embedded Studio page selects it.

Volume is a playback gain, not an edit to the stored soundtrack. Web Audio may
resample to the device's rate and apply device processing. Audible playback is
therefore not a certified-byte artifact; native WAV extraction and FMNP
certification remain separate output paths.

## Verification

`node --test scripts/test_studio_audio.cjs` exercises the production transport
and controller without npm packages. It checks actual WebCrypto hashes and
Response streams, clock boundaries, cancellation races, bounded lookahead,
generation changes, device suspension, EOF and preview request cadence. Repeated
rebuffering is checked by reconstructing both original PCM channels across
multiple delayed refills. The embedded-page script is also executed with minimal
DOM/event adapters to check its buffering button, ownership checks, cancellation
listeners, and fail-closed transport errors. Device scheduling is replaced by a
controllable clock: these are not real-browser or physical audio-device
acceptance results.

The Rust embedded-asset test ensures the controls and player ship in the same
versioned `/studio.js` asset without adding a filesystem-serving route. The
native Studio integration workflow retains its existing Rust/HTTP tests and
also executes the controller suite.
