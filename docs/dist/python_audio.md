# Sound assets in Python renders

`Scene.add_sound` accepts native PCM/float WAV and, in standard-mode host
renders, FLAC, MP3, ADTS AAC, M4A/MP4, Ogg Vorbis/Opus, and AIFF. Recognition
uses the file bytes, not the filename extension. Inputs retain their source
sample rate, channels, and explicit surround speaker layout until Reel's native
sound mixer performs channel mapping, resampling, timeline placement, gain,
and ducking. Soundtracks are published as mono or stereo.

```python
from manimlib import Scene
from fmn_python import render_scene

class NarratedScene(Scene):
    def construct(self):
        self.wait(0.5)
        self.add_sound("narration.mp3", gain=-3)
        self.wait(5)

receipt = render_scene(NarratedScene, "narrated.mp4")
print(receipt.audio_inputs)
```

For a soundtrack without video, select WAV:

```bash
fmn-python lesson.py NarratedScene --format wav --output narration.wav
fmn-python lesson.py First Second --format wav --ffmpeg_bin '/opt/media tools/ffmpeg' --video_dir soundtracks
```

## Generated and in-memory sound

`fmn_python.SoundData` freezes flat interleaved PCM samples into an immutable
native asset. It accepts mono, stereo, or explicitly described surround audio,
preserves the source sample rate, and copies the samples so later NumPy
mutations cannot change a queued sound.

```python
import numpy as np
from manimlib import Scene
from fmn_python import SoundData, render_scene

t = np.arange(4800) / 48000
beep = SoundData(0.2 * np.sin(2 * np.pi * 440 * t), sample_rate=48000)

class GeneratedSound(Scene):
    def construct(self):
        self.add_sound(beep, gain=-3)  # Relative to the current scene frame.
        self.file_writer.add_audio_segment(beep, time=1.0)  # Absolute seconds.
        self.file_writer.add_audio_segment(beep)  # Append at the soundtrack end.
        self.wait(2)

render_scene(GeneratedSound, "generated.wav")
```

For stereo arrays shaped `(frames, 2)`, pass `samples.ravel()` and `channels=2`.
`SoundData.from_wav_bytes(data)` accepts owned PCM WAV bytes. Neither path needs
a temporary source file or ffmpeg. Constructors validate finite samples and
admit at most 16,777,216 interleaved samples and 64 MiB before allocating native
PCM; `max_samples` and `max_bytes` can set smaller limits.

### Explicit surround layouts

Raw samples with more than two channels require the keyword-only
`channel_mask` argument. It is a Python integer containing the WAV speaker
mask, and the number of speakers must match `channels`. For example, a 5.1
array in front-left, front-right, center, LFE, back-left, back-right order is:

```python
surround = SoundData(samples.ravel(), sample_rate=48000,
                     channels=6, channel_mask=0x3f)
assert surround.channel_mask == 0x3f
```

Samples follow increasing WAV speaker-bit order. Supported layouts are:

| Layout | Channels | `channel_mask` | Interleaved speaker order |
| --- | ---: | ---: | --- |
| Mono | 1 | `None`, `0`, or `0x4` | Center |
| Stereo | 2 | `None`, `0`, or `0x3` | Front left, front right |
| 2.1 | 3 | `0xb` | Front left, front right, LFE |
| 3.0 | 3 | `0x7` | Front left, front right, center |
| Quad | 4 | `0x33` | Front left, front right, back left, back right |
| 4.0 | 4 | `0x107` | Front left, front right, center, back center |
| 5.0 back | 5 | `0x37` | Front left, front right, center, back left, back right |
| 5.0 side | 5 | `0x607` | Front left, front right, center, side left, side right |
| 5.1 back | 6 | `0x3f` | Front left, front right, center, LFE, back left, back right |
| 5.1 side | 6 | `0x60f` | Front left, front right, center, LFE, side left, side right |
| 6.1 | 7 | `0x70f` | Front left, front right, center, LFE, back center, side left, side right |
| 7.1 | 8 | `0x63f` | Front left, front right, center, LFE, back left, back right, side left, side right |

Missing, unknown, or mismatched surround masks fail before reading the sample
sequence. Booleans and non-integer masks fail with `TypeError`; convert a NumPy
integer mask with `int(mask)`. Negative masks and values above `0xffffffff`
fail with `OverflowError`. Mono and stereo keep their previous canonical WAV
bytes and hashes, including when their conventional mask is explicitly given;
their read-only `channel_mask` property is `None`.

Surround assets retain the explicit mask in their canonical float WAV and
through `from_wav_bytes`, copying, and queued playback. Their byte budget
includes a 68-byte extensible WAV header, followed by four bytes per interleaved
sample. Mono and stereo retain the 44-byte header. Both sample and byte budgets
are checked before sample iteration.

For stereo output, front left and front right have unity gain. Center is sent
to both channels at `1 / sqrt(2)`; each back or side speaker is sent to its side
at the same gain; back center is sent to both at `0.5`. LFE is excluded. Mono
output averages the resulting left and right channels. Mapping does not apply
an extra normalization or clip before the mixer's existing gain, resampling,
and final clipping policy. `writer.audio_segment` exposes the resulting
48 kHz stereo snapshot; its `channel_mask` is `None`.

### Writer timing and snapshots

The writer's `add_sound` accepts file paths or `SoundData`; `add_audio_segment`
accepts `SoundData`. Both use absolute timestamps, or append when `time` is
omitted. Append queries native duration metadata without materializing a mixed
waveform. The append position is the current end on the writer's 48 kHz sample
grid and is retained as an exact rational timestamp when exporting at another
rate. Resampling rounds source lengths up on this canonical grid, so append
does not promise gap-free concatenation at every export rate: a one-sample
44.1 kHz source occupies two 48 kHz writer frames, and its successor starts at
sample two in a 44.1 kHz export. An explicit `time` retains the supplied float
and is rounded once on the export sample grid.

`writer.audio_segment` explicitly requests an immutable mixed
`SoundData` snapshot, subject to its 64 MiB and preparation limits. Read
`sample_rate`, `channels`, `channel_mask`, `sample_frames`, `duration_seconds`,
`sha256`, or `wav_bytes()` from a source or snapshot. `init_audio()` clears inactive cue state;
`create_audio_segment()` clears it and exposes an empty snapshot. Accepted cues
cannot be cleared during an active output generation.

## File decoding and publication

The decoder uses `SceneFileWriter.ffmpeg_bin`, including explicit paths with
spaces. Its host search path and temporary-directory root are captured when
the render generation opens, before authored lifecycle callbacks run. Relative
sound paths are resolved when their cues are recorded in an active generation,
so later `chdir` calls do not redirect pending sound requests.

PCM WAV never resolves, probes, or launches ffmpeg. Compressed inputs require
the optional governed ffmpeg capability; a missing capability names PCM WAV as
the native-only alternative. The decoder uses a seekable snapshot in its
private work directory, an explicitly selected demuxer, a local-only protocol
allowlist, and disabled external MOV track references. Playlists are not audio
assets. It produces float32 PCM without filters, resampling, or an implicit
channel downmix. A recognized explicit WAV speaker mask is required for more
than two decoded channels, including when ffmpeg produces the intermediate
WAV. Ambiguous or unsupported layouts fail before mixing.

Every source is limited to 64 MiB. The native sample budget and independent
bounded reads also limit decompression. Reaching the output cap is an error,
not a successful truncated soundtrack. Source, decode, mix, or mux failures do
not replace an existing destination. The ordinary ordered video generation and
native WAV publisher retain ownership of final artifact publication.

## Receipts and reproducibility

`RenderResult.audio_inputs` contains per-cue source path, source SHA-256,
normalized PCM SHA-256, recognized container, source sample rate, channels,
sample-frame count, decoder kind, and optional decoder binary SHA-256.
Memory inputs use `source_kind="memory-wav"`, `path=None`, and a
`memory:sha256:...` source identity; their source digest hashes the immutable
canonical WAV bytes.
`ffmpeg_invocations` includes compressed audio decoding even for WAV-only
exports. Movie receipts retain video encoding, audio decoding, and final mux
facts. `as_dict()` returns independent copies suitable for JSON consumers.

The PCM digest hashes a versioned, little-endian normalized stream, including
sample count and format metadata, with negative zero canonicalized. Different
WAV sample packings can have the same decoded PCM identity while retaining
different source identities. Mono and stereo keep their version-1 PCM digest;
surround uses version 2, which also binds the explicit speaker mask. Receipt
fieldsets stay unchanged: the digest binds layout identity without adding a
field to existing durable receipts. Bundle checkpoint restoration accepts both
file and memory sources through eight channels, while continuing to validate
the final soundtrack as mono or stereo and charge the full decoded source
width against its PCM budget. The mixer remains native and thread-count
independent. **Compressed decoding is not certified across ffmpeg versions or
platforms**, and these receipts do not enable the still-incomplete certified
Python input closure. The portal continues to report `certified=False`.
