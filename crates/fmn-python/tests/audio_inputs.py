"""Real Scene.add_sound decoding, native mix, atomic publication and receipts."""
import contextlib
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import runpy
import shutil
import struct
import subprocess
import tempfile
import wave

import numpy as np
import manimlib as m
from fmn_python import SoundData, export_bundle, record_scene, render_scene, render_session
from fmn_python.console_rendering import try_render_cli


def float_wav(path, samples, channels, rate):
    payload = samples if isinstance(samples, bytes) else np.asarray(samples, dtype="<f4").tobytes()
    fmt = struct.pack("<HHIIHH", 3, channels, rate, rate * channels * 4, channels * 4, 32)
    path.write_bytes(b"RIFF" + struct.pack("<I", 36 + len(payload)) + b"WAVEfmt " +
                     struct.pack("<I", 16) + fmt + b"data" + struct.pack("<I", len(payload)) + payload)


def sound_scene(asset, executable="ffmpeg"):
    class Sound(m.Scene):
        def construct(self):
            self.wait(1 / 8)
            self.add_sound(str(asset), gain=-3)
            self.wait(1 / 16)
            self.add_sound(str(asset), time_offset=-4 / 48000, gain=-12, gain_to_background=-6)
            self.wait(5 / 16)
    return Sound(file_writer_config={"ffmpeg_bin": str(executable)})


def render(asset, destination, executable="ffmpeg", threads=1):
    return render_scene(sound_scene(asset, executable), destination,
                        resolution=(96, 54), fps=16, threads=threads)


def pcm(path):
    with wave.open(str(path), "rb") as audio:
        assert (audio.getframerate(), audio.getnchannels(), audio.getsampwidth()) == (48000, 2, 2)
        return np.frombuffer(audio.readframes(audio.getnframes()), dtype="<i2").reshape(-1, 2)


def memory_sound_inputs(root):
    """Caller mutations, paths and worker counts cannot change frozen PCM."""
    samples = np.array([0.125, -0.25, 0.0, -0.0] * 64, dtype=np.float32)
    source = SoundData(samples, sample_rate=48000, channels=2)
    expected = samples.copy()
    samples[:] = 0.75
    assert (source.channels, source.sample_rate, source.sample_frames) == (2, 48000, 128)
    assert source.duration_seconds == 128 / 48000
    assert source.sha256 == hashlib.sha256(source.wav_bytes()).hexdigest()
    assert SoundData.from_wav_bytes(source.wav_bytes()).sha256 == source.sha256
    assert copy.copy(source).sha256 == copy.deepcopy(source).sha256 == source.sha256
    assert SoundData([0.0]).sha256 == SoundData([-0.0]).sha256
    try:
        source.sample_rate = 1
    except AttributeError:
        pass
    else:
        raise AssertionError("frozen sound metadata was writable")
    for samples, options in (([float('nan')], {}), ([float('inf')], {}),
                             ([1e300], {}), ([True], {}), ([1], {'channels': 2}),
                             ([1], {'sample_rate': 0}), ([1], {'channels': 3}),
                             ([1], {'max_samples': 0}), ([1], {'max_bytes': 44}),
                             ([1], {'max_bytes': 67_108_865})):
        try:
            SoundData(samples, **options)
        except (ValueError, TypeError):
            pass
        else:
            raise AssertionError(f"invalid sound admitted: {samples!r}, {options!r}")

    class OverBudget:
        def __len__(self):
            return 16_777_217
        def __getitem__(self, index):
            raise AssertionError("sample read before size admission")
    try:
        SoundData(OverBudget())
    except ValueError:
        pass
    else:
        raise AssertionError("oversized PCM source was admitted")

    # Use independent file-backed PCM as the oracle for timing, gains, ducking
    # and resampling. Neither memory output may touch any path resolver.
    file_source = root / 'memory-equivalent.wav'
    float_wav(file_source, expected, 2, 48000)
    def cues(scene, asset):
        scene.wait_frames(1)
        scene.add_sound(asset, time_offset=-4 / 48000, gain=-3)
        scene.add_sound(asset, time_offset=60 / 48000, gain=-12, gain_to_background=-6)
        with scene.temp_skip():
            scene.add_sound(object(), time_offset=float('nan'))
        scene.wait_frames(1)
    class FileSound(m.Scene):
        def construct(self):
            cues(self, file_source)
    reference = root / 'memory-reference.wav'
    render_scene(FileSound, reference, fps=29, threads=1)
    sounds = __import__('manimlib.utils.sounds', fromlist=['get_full_sound_file_path'])
    resolver = sounds.get_full_sound_file_path
    def no_path(_):
        raise AssertionError("an in-memory cue called the path resolver")
    class MemorySound(m.Scene):
        def construct(self):
            cues(self, source)
    sounds.get_full_sound_file_path = no_path
    try:
        for threads in (1, 4, 16):
            destination = root / f'memory-{threads}.wav'
            scene = MemorySound(file_writer_config={'ffmpeg_bin': str(root / 'missing-memory-decoder')})
            result = render_scene(scene, destination, fps=29, threads=threads)
            assert destination.read_bytes() == reference.read_bytes()
            assert result.ffmpeg_invocations == ()
            assert len(result.audio_inputs) == 2
            for receipt in result.audio_inputs:
                assert receipt['path'] is None and receipt['source_kind'] == 'memory-wav'
                assert receipt['source'] == 'memory:sha256:' + source.sha256
                assert receipt['source_sha256'] == source.sha256
                assert receipt['decoder'] == 'native-wav'
            assert all(fact[1:3] == (1, 29) for fact in scene._sound_request_facts())
    finally:
        sounds.get_full_sound_file_path = resolver


def writer_sound_inputs(root):
    values = np.arange(512, dtype=np.float32) / 4096
    source = SoundData(values, sample_rate=48000)
    expected_pcm = np.repeat((np.arange(512, dtype=np.int16) * 8)[:, None], 2, axis=1)
    scene = m.Scene(file_writer_config={'ffmpeg_bin': str(root / 'missing-writer-decoder')})
    writer = scene.file_writer
    assert writer.includes_sound is False and writer.audio_segment is None
    writer.create_audio_segment()
    assert writer.audio_segment.sample_frames == 0 and writer.includes_sound is False
    scene.wait_frames(7)
    writer.add_audio_segment(source)
    frozen = writer.audio_segment
    assert writer.includes_sound and frozen.sample_frames == 512
    assert frozen.sample_rate == 48000 and frozen.channels == 2
    writer.add_audio_segment(source)
    assert writer.audio_segment.sample_frames == 1024 and frozen.sample_frames == 512
    mixed = np.frombuffer(writer.audio_segment.wav_bytes(), dtype='<f4', offset=44).reshape(-1, 2)
    np.testing.assert_array_equal(mixed, np.repeat(np.tile(values, 2)[:, None], 2, axis=1))
    assert scene.get_frame_count() == 7, 'writer audio moved the scene clock'
    with scene.temp_skip():
        writer.add_audio_segment(object(), time=float('nan'))
        writer.add_sound(object(), time=float('nan'), gain=float('inf'))
    assert len(scene._sound_request_facts()) == 2
    for operation in (lambda: writer.add_audio_segment(object()),
                      lambda: writer.add_sound(source, time=float('nan')),
                      lambda: writer.add_sound(source, time=0, gain=float('inf'))):
        try:
            operation()
        except (TypeError, ValueError):
            pass
        else:
            raise AssertionError('invalid writer cue was admitted')
    assert len(scene._sound_request_facts()) == 2
    writer.init_audio()
    assert writer.audio_segment is None and not writer.includes_sound
    assert frozen.sample_frames == 512 and scene.get_frame_count() == 7

    # Appending uses native duration metadata, never a full stereo snapshot.
    # A 180-second 1 Hz source is tiny, but its 48 kHz stereo rendering exceeds
    # the explicit SoundData snapshot cap. Empty cues cannot extend its end.
    long_scene = m.Scene(file_writer_config={'ffmpeg_bin': str(root / 'missing-writer-decoder')})
    long_writer = long_scene.file_writer
    long_writer.add_audio_segment(SoundData([0.125] * 180, sample_rate=1))
    long_writer.add_audio_segment(SoundData([]), time=999999)
    assert long_scene._soundtrack_duration(long_writer) == 180.0
    long_writer.add_audio_segment(source)
    assert len(long_scene._sound_request_facts()) == 3
    assert long_scene._soundtrack_duration(long_writer) == (180 * 48000 + 512) / 48000

    # Absolute timestamps are independent of the nonzero authoring call site.
    # Inspect real published S16 PCM using source values with exact quantization.
    for threads in (1, 4):
        scene = m.Scene(file_writer_config={'ffmpeg_bin': str(root / 'missing-writer-decoder')})
        target = root / f'writer-absolute-{threads}.wav'
        with render_session(scene, target, fps=29, threads=threads):
            scene.wait_frames(3)
            scene.file_writer.add_audio_segment(source, time=5 / 48000)
            try:
                scene.file_writer.init_audio()
            except RuntimeError as error:
                assert 'active output' in str(error)
            else:
                raise AssertionError('active recording lost its queued audio')
            scene.wait_frames(1)
        samples = pcm(target)
        assert len(samples) == sample_boundary(4, 29)
        np.testing.assert_array_equal(samples[5:517], expected_pcm)
        assert not samples[:5].any() and not samples[517:].any()

    # Live insertion keeps a source which starts before the nonzero origin:
    # three samples are clipped, not the entire overlapping cue.
    scene = m.Scene()
    with render_session(scene, root / 'writer-lead-in.y4m', fps=29, resolution=(48, 32)):
        scene.wait_frames(3)
    with record_scene(scene, root / 'writer-insert.wav', threads=1) as recorded:
        scene.file_writer.add_sound(source, time=3 / 29 - 3 / 48000)
        scene.wait_frames(1)
    samples = pcm(recorded.result.destination)
    np.testing.assert_array_equal(samples[:509], expected_pcm[3:])
    assert not samples[509:].any()
    assert len(recorded.result.audio_inputs) == 1

    # Explicit writer onsets remove observed skipped sample windows. The
    # independent expectation uses integer endpoints on the 29-fps grid.
    scene = m.Scene()
    target = root / 'writer-skipped.wav'
    with render_session(scene, target, fps=29):
        scene.wait_frames(1)
        with scene.temp_skip():
            scene.wait_frames(2)
        scene.file_writer.add_sound(source, time=3 / 29 + 2 / 48000)
        scene.wait_frames(1)
    onset = sample_boundary(1, 29) + 2
    samples = pcm(target)
    np.testing.assert_array_equal(samples[onset:onset + 512], expected_pcm)
    assert not samples[:onset].any() and not samples[onset + 512:].any()

    # Native bundle soundtracks consume both timing classes and preserve the
    # same PCM regardless of whether the asset originated in a file or memory.
    asset = root / 'writer-file.wav'
    float_wav(asset, values, 1, 48000)
    file_scene = m.Scene(file_writer_config={'ffmpeg_bin': str(root / 'missing-writer-decoder')})
    file_scene.file_writer.add_sound(asset)
    file_scene.file_writer.add_sound(asset)
    assert file_scene._soundtrack_duration(file_scene.file_writer) == 1024 / 48000
    file_mix = np.frombuffer(file_scene.file_writer.audio_segment.wav_bytes(), dtype='<f4', offset=44)
    np.testing.assert_array_equal(file_mix.reshape(-1, 2), np.repeat(np.tile(values, 2)[:, None], 2, axis=1))
    def bundled(sound):
        class Audible(m.Scene):
            def construct(self):
                self.wait_frames(1)
                self.file_writer.add_sound(sound, time=7 / 48000, gain=-3)
                self.add_sound(sound, time_offset=-2 / 48000, gain=-6)
                self.wait_frames(1)
        return Audible
    memory = root / 'writer-memory.fmtl'
    file = root / 'writer-file.fmtl'
    options = dict(audio=True, fps=29, resolution=(48, 32))
    memory_result = export_bundle(bundled(source), memory, **options)
    file_result = export_bundle(bundled(asset), file, **options)
    assert memory.read_bytes() == file.read_bytes()
    assert all(row['path'] is None for row in memory_result.audio_inputs)
    assert all(row['path'] is not None for row in file_result.audio_inputs)

    # A default append retains its exact 48 kHz writer-grid position when the
    # bundle uses another sample rate. At 44.1 kHz, 400/48000 is the half-sample
    # tie 367.5 -> 368; an explicitly authored float is just below that tie.
    silence = SoundData([0.0] * 400, sample_rate=48000)
    tick = SoundData([0.5], sample_rate=44100)
    def cross_grid(explicit_time):
        class Appended(m.Scene):
            def construct(self):
                self.file_writer.add_audio_segment(silence)
                self.file_writer.add_audio_segment(tick, time=explicit_time)
                self.wait_frames(1)
        return Appended
    for name, time, expected_frames in (('exact', None, 369),
                                         ('explicit', 400 / 48000, 368)):
        result = export_bundle(
            cross_grid(time), root / f'writer-cross-grid-{name}.fmtl',
            audio=True, audio_sample_rate=44100, audio_channels=1,
            fps=120, resolution=(48, 32),
        )
        assert result.soundtrack.sample_rate == 44100
        assert result.soundtrack.sample_frames == expected_frames


def sample_boundary(frame, fps):
    return (2 * frame * 48000 + fps) // (2 * fps)


with tempfile.TemporaryDirectory(prefix="fmn sound inputs ") as directory:
    root = Path(directory)
    memory_sound_inputs(root)
    writer_sound_inputs(root)
    native_asset = root / "native.wav"
    float_wav(native_asset, [0.125, -0.25] * 128, 2, 48000)
    # A missing optional tool cannot turn PCM-only WAV export into a capability
    # failure. Its native decode is still named in the artifact receipt.
    native = render(native_asset, root / "native-output.wav", root / "missing-ffmpeg")
    assert native.sample_frames == 24000 and native.certified is False
    assert native.ffmpeg_invocations == ()
    assert len(native.audio_inputs) == 2
    for fact in native.audio_inputs:
        assert fact["decoder"] == "native-wav" and fact["decoder_sha256"] is None
        assert fact["source_sha256"] == hashlib.sha256(native_asset.read_bytes()).hexdigest()
        assert fact["sample_frames"] == 128
    facts = native.as_dict()
    facts["audio_inputs"][0]["source_sha256"] = "changed-copy"
    assert native.audio_inputs[0]["source_sha256"] != "changed-copy"

    # Reject writer callbacks that record a cue during preflight rather than
    # replacing their scene engine and silently losing the cue.
    side_effect_scene = m.Scene()
    class RecordingWriter:
        @property
        def ffmpeg_bin(self):
            side_effect_scene.add_sound(str(native_asset))
            return "ffmpeg"
    side_effect_scene.file_writer = RecordingWriter()
    try:
        side_effect_scene._begin_native_output(str(root / "preflight.wav"), "wav", 96, 54, 16, 1, 0)
    except RuntimeError as error:
        assert "mutates engine state" in str(error), error
    else:
        raise AssertionError("writer callback's sound state was erased")
    assert len(side_effect_scene._sound_request_facts()) == 1
    assert not (root / "preflight.wav").exists()

    # Path resolution belongs to the cue call site, not whichever cwd a later
    # authored callback happens to leave active when the render finishes.
    previous_cwd = Path.cwd()
    elsewhere = root / "elsewhere"
    elsewhere.mkdir()
    class ChangingCwd(m.Scene):
        def construct(self):
            os.chdir(root)
            self.add_sound("native.wav")
            os.chdir(elsewhere)
            self.wait(1 / 8)
    try:
        result = render_scene(ChangingCwd, root / "cwd.wav", fps=16, threads=1)
    finally:
        os.chdir(previous_cwd)
    # Canonical receipt paths may expand a Windows short name or namespace.
    receipt_path = Path(result.audio_inputs[0]["path"])
    assert receipt_path.is_absolute() and ".." not in receipt_path.parts
    assert receipt_path.samefile(native_asset)

    ffmpeg = shutil.which("ffmpeg")
    if not ffmpeg:
        assert not os.environ.get("FMN_REQUIRE_FFMPEG"), "required ffmpeg is unavailable"
        print("portal-audio: native WAV and ownership verified; external decode unavailable")
    else:
        fixtures = root / "fixtures"
        runpy.run_path(_audio_fixture_generator)["make"](fixtures)
        # A real native executable at a path containing spaces, not shell
        # tokenization, is the same configured capability for WAV and movies.
        explicit = root / "decoder with spaces"
        shutil.copyfile(ffmpeg, explicit)
        explicit.chmod(0o700)
        tool_hash = hashlib.sha256(explicit.read_bytes()).hexdigest()
        for row in (fixtures / "manifest.tsv").read_text().splitlines():
            asset_name, reference_name, channels, rate = row.split("\t")
            asset = fixtures / asset_name
            baseline = root / f"{asset_name}.reference.wav"
            float_wav(baseline, (fixtures / reference_name).read_bytes(), int(channels), int(rate))
            golden_path = root / f"{asset_name}.golden.wav"
            render(baseline, golden_path, root / "unneeded-decoder")
            outputs = []
            for threads in (1, 4):
                destination = root / f"{asset_name}.{threads}.wav"
                receipt = render(asset, destination, explicit, threads)
                assert destination.read_bytes() == golden_path.read_bytes(), asset_name
                assert receipt.sample_frames == 24000
                assert len(receipt.ffmpeg_invocations) == 2
                assert len(receipt.audio_inputs) == 2
                for fact in receipt.audio_inputs:
                    assert fact["source_sha256"] == hashlib.sha256(asset.read_bytes()).hexdigest()
                    assert fact["channels"] == int(channels) and fact["sample_rate"] == int(rate)
                    assert fact["decoder"] == "ffmpeg" and fact["decoder_sha256"] == tool_hash
                    assert len(fact["pcm_sha256"]) == 64
                for invocation in receipt.ffmpeg_invocations:
                    assert invocation["tool_sha256"] == tool_hash
                    assert invocation["tool_path"] == str(explicit)
                    assert invocation["encoder"] == "pcm_f32le"
                    assert "-ar" not in invocation["argv"] and "-ac" not in invocation["argv"]
                outputs.append(receipt.digest)
            assert outputs[0] == outputs[1], "thread count changed native mixed WAV"

        # Captured PATH survives authored callbacks. PCM-only operation above
        # proves this does not eagerly probe ffmpeg for every generation.
        old_path = os.environ.get("PATH")
        class ChangedPath(m.Scene):
            def construct(self):
                os.environ["PATH"] = str(root / "empty-bin")
                self.add_sound(str(fixtures / "flac.asset"))
                self.wait(1 / 8)
        try:
            result = render_scene(ChangedPath, root / "captured-path.wav", fps=16, threads=1)
        finally:
            if old_path is None:
                os.environ.pop("PATH", None)
            else:
                os.environ["PATH"] = old_path
        assert result.audio_inputs[0]["decoder"] == "ffmpeg"

        # Decoder failures preserve an already existing destination. No cue is
        # silently dropped and no successful-looking truncated WAV is emitted.
        bad = root / "bad.flac"
        bad.write_bytes(b"fLaCbroken")
        playlist = root / "playlist.m3u"
        playlist.write_text("#EXTM3U\nhttps://example.invalid/voice.mp3\n")
        for asset, executable in [(bad, explicit), (playlist, root / "missing-decoder"),
                                  (fixtures / "flac.asset", root / "missing-decoder")]:
            destination = root / "preserved.wav"
            destination.write_bytes(b"existing soundtrack")
            try:
                render(asset, destination, executable)
            except (RuntimeError, ValueError, OSError):
                pass
            else:
                raise AssertionError("invalid audio was silently accepted")
            assert destination.read_bytes() == b"existing soundtrack"

        # The complete movie path consumes compressed cues, publishes audio and
        # video together, and records the decoder between encode and mux facts.
        for extension in ("mp4", "mov"):
            destination = root / f"soundtrack.{extension}"
            receipt = render(fixtures / "flac.asset", destination, explicit)
            assert receipt.frame_count == 8 and len(receipt.ffmpeg_invocations) == 4
            assert receipt.ffmpeg_invocations[0]["encoder"] == "libx264"
            assert [v["encoder"] for v in receipt.ffmpeg_invocations[1:3]] == ["pcm_f32le"] * 2
            assert "copy" in receipt.ffmpeg_invocations[-1]["argv"]
            decoded = subprocess.run([ffmpeg, "-v", "error", "-i", str(destination),
                                      "-map", "0:a:0", "-c:a", "pcm_s16le", "-f", "s16le", "pipe:1"],
                                     capture_output=True, check=True, timeout=30).stdout
            samples = np.frombuffer(decoded, dtype="<i2").reshape(-1, 2)
            expected = pcm(root / "flac.asset.golden.wav")
            assert 24000 <= len(samples) < 25024
            assert np.sqrt(np.mean(samples[1000:5000].astype(float) ** 2)) < 32
            for channel in (0, 1):
                assert np.corrcoef(samples[6500:11000, channel], expected[6500:11000, channel])[0, 1] > 0.97

        # Both single and batch console owners accept the audio decoder flag,
        # including paths with spaces, without constructor keyword rewrites.
        source = root / "scene.py"
        source.write_text("from manimlib import *\nclass First(Scene):\n"
                          "    def construct(self):\n"
                          f"        self.add_sound({str(fixtures / 'mp3.asset')!r})\n"
                          "        self.wait(0.25)\nclass Second(First):\n    pass\n")
        for names, target in [(["First"], root / "console.wav"),
                              (["First", "Second"], root / "batch")]:
            # --output is one scene's exact destination; a batch's root is
            # --video_dir (BN-15, the console's destination contract).
            destination = ["--output" if len(names) == 1 else "--video_dir", str(target)]
            stdout, stderr = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                code = try_render_cli(m, ["--robot", str(source), *names, "--format", "wav",
                                         "--fps", "16", "--threads", "1", "--ffmpeg_bin", str(explicit),
                                         *destination])
            assert code == 0, (stdout.getvalue(), stderr.getvalue())
            report = json.loads(stdout.getvalue())
            assert report["exit"]["code"] == 0
            if len(names) == 1:
                assert len(report["audio_inputs"]) == 1 and len(report["ffmpeg_invocations"]) == 1
            else:
                assert (target / "First.wav").is_file() and (target / "Second.wav").is_file()
        print("portal-audio: eight compressed formats, 1/4-thread mixes, WAV/movie/console/batch and failure preservation verified")
