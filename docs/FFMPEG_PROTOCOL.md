# FFMPEG_PROTOCOL.md — v2

The contract of FrankenManim's **one external tool** (§3 D2, §14.3,
D-23). ffmpeg is the only subprocess the engine will ever invoke —
encode, mux, transcode — and this document is the protocol every
invocation obeys. Implementation: `fmn-output::negotiate` (pure argv
construction) and `fmn-output::ffmpeg` (sandboxed execution) over
`fmn-platform::process` (the argv-only mechanism).

## 1. Negotiation, not a fixed pipe

v1-era manim pipes `rawvideo/rgba`, then repairs orientation with
`vflip` and color with `eq`. v2 deletes both repairs structurally:

- **Orientation.** fmn-frame renders in output orientation (row 0 is
  the top row, D-23). No argv builder can emit `vflip`; the contract
  suite asserts no filter argument exists in any invocation.
- **Transfer.** fmn-frame applies the transfer function once,
  natively. Color metadata (`-color_primaries`, `-color_trc`,
  `-colorspace`, `-color_range`) *describes* the bytes; no `eq`
  correction exists.

Negotiated per job:

| Dimension | Values | Wire meaning |
|---|---|---|
| Pixel format | `rgba`, `bgra` (alpha/compat), `nv12` (8-bit video), `p010le` (10-bit) | tightly-packed frames on stdin, frame-index order |
| Frame rate | exact rational `num/den` | from the RationalFrameClock, no float drift |
| Color | primaries BT.709; transfer `iec61966-2-1` or `bt709`; range `tv`/`pc` | metadata only, never a correction |
| Container | MP4 (`+faststart`), MOV, transparent MOV (`qtrle`/argb, requires an alpha wire), GIF mode | |
| Encoder | `Auto` → software default (`libx264`; `qtrle` for transparent MOV); hardware by explicit name only | identity → provenance |

The arithmetic that motivates NV12: 3840×2160 RGBA8 is 33,177,600
bytes/frame against NV12's 12,441,600 — 2.67× less pipe payload
(~1.99 GB/s vs ~746 MB/s at 60 fps) before counting copies.

Refused negotiations are typed and named: transparent MOV on an
opaque wire, CRF on a non-CRF encoder, an encoder the installed
ffmpeg does not offer, a payload that is not a whole number of
frames, zero dimensions or frame rate.

## 2. The security protocol (D2, complete)

Every invocation:

1. **argv-only.** `ProcessSpec` is an absolute program path plus an
   argument vector. No shell exists in the API; relative paths are
   refused by the mechanism itself (no ambient `PATH` can choose the
   executable).
2. **Private working directory.** Each job gets a fresh directory;
   the child's `cwd` and `TMPDIR` point into it; the artifact is born
   there.
3. **Environment allowlist + locale pinning.** The child environment
   is cleared and rebuilt as exactly `LANG=C`, `LC_ALL=C`,
   `TMPDIR=<private dir>`.
4. **Timeout + cancellation.** A wall-clock bound kills the child on
   expiry. (Tree-kill honesty: the std mechanism kills the direct
   child; ffmpeg does not daemonize under this contract — see
   `fmn-platform::process` docs for the revisit path.)
5. **Output-size limits.** Captured logs are capped per stream
   (overflow kills the child); the artifact is size-checked against a
   declared budget before publication.
6. **Atomic publication.** The artifact reaches its destination only
   through `rename` after verification. A failed, timed-out, or
   oversized job leaves the destination untouched.
7. **Provenance.** Tool path **and content hash** (SHA-256 of the
   executable bytes), `-version` line, resolved encoder, and the full
   argv are recorded on every job.

## 3. Optionality

ffmpeg's absence is a **capability error naming the alternative**:
y4m, PNG sequences, and GIF are native outputs needing no ffmpeg;
only encoded video (mp4/mov), the audio mux, and media transcode
require the tool. There is no silent format substitution, ever.

## 4. Retained modes

- **GIF mode** — `-f gif`, muxer-level, no `-c:v` (parity with the
  Reference; the native GIF codec is the default path).
- **Two-stage audio mux** — stage 1 encodes video; stage 2 runs
  `-i video -i audio -c:v copy -c:a aac -map 0:v:0 -map 1:a:0`.
  Stage 2 **must not re-encode video**: `-c:v copy` is contract,
  asserted by the fake-ffmpeg suite.
- **Insert files / partial movies** — concat demuxer with stream
  copy (`-f concat -safe 0 -i list -c copy`); input paths containing
  quotes or newlines are refused rather than escaped.
- **`--subdivide` outputs** — one boundary job per subdivision; the
  protocol is per-invocation and needs nothing special.
- **`--prerun` counting** — a counting pass invokes the boundary
  zero times; the contract suite asserts no spawn occurs.
- **Media transcode as a capability** — audio decode beyond WAV
  (`-vn -acodec pcm_s16le -f wav`) and exotic image formats
  (`-frames:v 1 -c:v png`) ride the same sandbox and the same
  fingerprinting; absence yields the same named capability error.

## 5. Hardware encoders

Hardware encoders enter **here and only here**: ffmpeg products are
excluded from certification by construction, so hardware encode
changes nothing about the determinism story. Recognized names:
`h264_videotoolbox`, `hevc_videotoolbox`, `prores_videotoolbox`,
`h264_nvenc`, `hevc_nvenc`, `av1_nvenc`. Policy: `Auto` always
resolves to the software default; hardware is selected only by
explicit name, validated against the probed `-encoders` inventory,
and recorded in provenance. `fmn doctor` reports the installed
ffmpeg's fingerprint and its recognized hardware encoders.

## 6. CI: the fake ffmpeg

The protocol is CI-verified without real encoders, at two layers:

- **Contract suite** (`ScriptedRunner`): every negotiation dimension,
  the env allowlist, both mux stages, and the failure modes (timeout,
  log overflow, nonzero exit, missing/oversized artifact) are
  asserted against recorded `ProcessSpec`s — no process spawns.
- **Sandbox suite** (fake-ffmpeg script + the real `StdProcessRunner`):
  a scripted stand-in binary asserts the spawn-level behavior —
  artifact publication, stdin consumption, timeout kill, oversized-
  artifact refusal, failed-job destination integrity.
- **Real-ffmpeg smoke test**, behind `FMN_REAL_FFMPEG=1`, encodes a
  short NV12 clip through the installed tool.
