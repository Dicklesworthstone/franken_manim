# Sound assets in Python renders

`Scene.add_sound` accepts native PCM/float WAV and, in standard-mode host
renders, FLAC, MP3, ADTS AAC, M4A/MP4, Ogg Vorbis/Opus, and AIFF. Recognition
uses the file bytes, not the filename extension. Mono and stereo inputs retain
their source sample rate and channels until Reel's native sound mixer performs
resampling, timeline placement, gain, and ducking.

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
native asset. It accepts mono or stereo audio, preserves the source sample rate,
and copies the samples so later NumPy mutations cannot change a queued sound.

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
`sample_rate`, `channels`, `sample_frames`, `duration_seconds`, `sha256`, or
`wav_bytes()` from a source or snapshot. `init_audio()` clears inactive cue state;
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
channel downmix; multichannel audio is rejected rather than silently changed.

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
different source identities. The mixer remains native and thread-count
independent. **Compressed decoding is not certified across ffmpeg versions or
platforms**, and these receipts do not enable the still-incomplete certified
Python input closure. The portal continues to report `certified=False`.
