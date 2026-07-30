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

## 2. The D2 invocation protocol

### 2.0 Audited executable discovery

Executable discovery is an fmn-platform capability, not process-runner
behavior. The host snapshots one explicit native `PATH` value when it
constructs `FfmpegLocator`; neither the locator nor any consumer rereads the
environment. An absolute configured path bypasses `PATH` and is canonicalized
exactly as supplied. A relative configuration may be only the fixed bare
`ffmpeg` name (`ffmpeg.exe` is also accepted on Windows).

Before searching, the locator validates the complete path list. Empty entries
(which conventional lookup can reinterpret as the current directory),
relative entries, parent traversal, non-UTF-8 or control-bearing entries,
embedded NUL, malformed Windows quoting, and bounded-input overruns reject the
whole policy before any candidate is inspected. Search order is otherwise
preserved. A present directory, broken link, non-executable file, or
interpreter script is skipped; the first regular host-native executable image
wins. Linux-family hosts require ELF, macOS accepts the declared thin/fat
Mach-O magics, and Windows validates bounded DOS/PE headers. Other hosts refuse
discovery until their native image format is explicitly governed. Windows
search tests exactly `ffmpeg.exe` in each validated directory and never
consults `PATHEXT`, the application/current/system directories, or
command-interpreter formats.

Symlinks are supported for ordinary package-manager/version-manager layouts,
but the issued `FfmpegExecutable` contains their canonical target. Retargeting
the searched symlink afterward therefore cannot redirect the issued path.
This is still selection-time format validation over a pathname, not a byte
identity or proof of authorship: fmn-output owns the hash/private-copy binding
below and must revalidate the exact private copy in the complete D2 boundary.
Its resolution probe requires a strict UTF-8 first line that begins exactly
`ffmpeg version ` and contains no control characters. Native-image shape plus
that protocol response proves only that the selected bytes speak the governed
surface; it is not cryptographic authentication of the ffmpeg project.

Every invocation:

1. **argv-only, private-copy binding.** The configured tool is
   canonicalized and SHA-256 fingerprinted. Resolution copies those
   bytes through `create_new` into a private probe directory, verifies
   the copy, and runs `-version` from that copy. Every later capability
   probe and job repeats the exact-create/copy/hash operation and passes
   only the absolute private path to `ProcessSpec`. The private copy is
   rehashed after execution; the configured pathname never selects what
   the process mechanism spawns. No shell or ambient `PATH` exists.
2. **Owned private hierarchy.** Resolution canonicalizes the caller's
   workdir parent and atomically claims one session root. Each probe and
   job then claims an exclusive child (`0700` on Unix). A collision is
   never opened, cleaned, or reused. The recorded filesystem identity of
   the session and each child must still match before later path-based
   work or cleanup; mismatch retains the path untouched. The child's
   `cwd` and `TMPDIR`, artifact, and bound executable all live there.
3. **Environment allowlist + locale pinning.** The child environment
   is cleared and rebuilt as exactly `LANG=C`, `LC_ALL=C`,
   `TMPDIR=<job dir>`.
4. **Timeout + cancellation.** A wall-clock bound or cooperative
   cancellation kills the complete isolated process group. Targets
   without a safe process-tree mechanism are refused before spawn.
5. **Output-size limits.** Captured logs are capped per stream
   (overflow kills the child); the artifact is size-checked against a
   declared budget before publication.
6. **Atomic publication.** The artifact reaches its destination only
   through `rename` after verification. A failed, timed-out, or
   oversized job leaves the destination untouched.
7. **Provenance.** Every invocation records the canonical configured
   path, the private executable path actually spawned, their enforced
   shared SHA-256, the `-version` line, resolved encoder, and full argv.
   The private executable is rehashed after the process exits and before
   its artifact can publish.

### 2.1 Supported executable and filesystem capability

The same private-copy form used by jobs is what earns the resolution-time
version. This deliberately supports self-contained or relocation-safe
ffmpeg builds. On the currently supported Unix boundary, an installation
whose loader requires libraries beside the configured executable—for example
through ELF `$ORIGIN` or Mach-O `@executable_path`—is rejected as
`UnsupportedRelocatedExecutable` if the private-copy probe cannot run. There
is no fallback to probing or executing the mutable configured path.

On Unix, missing workdir-parent components and every claimed directory are
created as mode `0700`. A canonical ancestor writable by group/other is
accepted only when it has the sticky bit (as `/tmp` normally does).
Parent-directory (`..`) components are refused before creation. The current
non-Unix implementation fails earlier with `Workdir`, before any executable
probe, because safe `std` cannot prove a private directory ACL there. A future
Windows host capability must also account for application-directory loader
lookup when it enables private-copy execution. This is separate from the
process mechanism's own fail-closed requirement for complete process-tree
cancellation.

Safe `std` does not expose a portable execute-by-handle primitive or
directory-handle-relative recursive deletion. The caller must supply trusted
workdir ancestry; within it, the private hierarchy, content hashes, and
filesystem-identity checks prevent ambient-path substitution and make
persistent replacement fail closed. A hostile owner of that ancestry or an
actor already running under the same OS identity can still race a pathname
between the last check and the OS spawn/removal operation. Supporting that
stronger threat model requires an opaque host executable/filesystem capability;
it is not papered over by a weaker or unsafe platform path.

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
  log overflow, nonzero exit, missing/oversized artifact), executable
  substitution, relocation refusal, exclusive-directory collision
  handling, and fail-closed cleanup after directory replacement are
  asserted against recorded `ProcessSpec`s — no process spawns.
- **Sandbox suite** (fake-ffmpeg script + the real `StdProcessRunner`):
  a scripted stand-in binary asserts the spawn-level behavior —
  artifact publication, stdin consumption, timeout kill, oversized-
  artifact refusal, failed-job destination integrity, and that source
  replacement after binding cannot change the executable that runs.
- **Real-ffmpeg smoke test**, behind `FMN_REAL_FFMPEG=1`, encodes a
  short NV12 clip through the installed tool.
