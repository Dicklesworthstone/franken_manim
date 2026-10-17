"""Real Python scenes -> native mixer -> self-contained FMTL; no mocked engine."""
from pathlib import Path
import hashlib
import struct
import tempfile
import wave
import manimlib as m
from fmn_python import BundleExportSession, export_bundle, render_scene


def write_wav(path, channels, rate, samples):
    with wave.open(str(path), "wb") as output:
        output.setnchannels(channels)
        output.setsampwidth(2)
        output.setframerate(rate)
        output.writeframes(struct.pack("<" + "h" * len(samples), *(round(x * 32768) for x in samples)))


with tempfile.TemporaryDirectory(prefix="fmn-python-audible-") as temporary:
    root = Path(temporary)
    background = root / "background.wav"
    lead = root / "lead.wav"
    short = root / "short.wav"
    write_wav(background, 1, 8, [0.25] * 8)
    write_wav(lead, 2, 8, [0.25, -0.25, 0.5, -0.5])
    write_wav(short, 1, 8, [0.5])

    class Narrated(m.Scene):
        sound = True
        move_camera = False
        def construct(self):
            self.construct_calls = getattr(self, "construct_calls", 0) + 1
            self.updates = 0
            dot = m.Square(side_length=0.5, fill_opacity=1)
            def update(mob, dt):
                self.updates += 1
                mob.shift(dt * m.RIGHT)
            dot.add_updater(update)
            self.add(dot)
            if self.sound:
                self.add_sound(str(background))
            self.wait(0.25)
            if self.move_camera:
                self.camera.frame.shift(m.RIGHT)
            if self.sound:
                self.add_sound(str(lead), time_offset=-0.125,
                               gain=-6.020599913279624, gain_to_background=-6.020599913279624)
            self.wait(0.25)

    expected = [0.25, 0.25, 0.25, 0.0, 0.375, -0.125] + [0.25, 0.25] * 5
    for camera in (False, True):
        class Selected(Narrated):
            move_camera = camera
        class Silent(Selected):
            sound = False
        silent = root / f"silent-{camera}.fmtl"
        old = export_bundle(Silent, silent, camera=camera, resolution=(64, 36), fps=8)
        aware = root / f"silent-aware-{camera}.fmtl"
        no_sound = export_bundle(Silent, aware, audio=True, camera=camera, resolution=(64, 36), fps=8)
        assert silent.read_bytes() == aware.read_bytes()
        assert no_sound.soundtrack is None and _test_bundle_pcm(aware.read_bytes()) is None
        replay = _test_camera_bundle_frames if camera else _test_bundle_frames
        pictures = replay(silent.read_bytes(), list(range(4)), 64, 36, 1)
        reference = None
        for workers in (1, 4, 16):
            scene = Selected()
            destination = root / f"sound-{camera}-{workers}.fmtl"
            result = export_bundle(scene, destination, audio=True, camera=camera,
                                   audio_sample_rate=8, audio_threads=workers, resolution=(64, 36), fps=8)
            data = destination.read_bytes()
            assert result.digest == hashlib.sha256(data).hexdigest()
            assert result.frame_count == 4 and result.segment_count == 2
            assert result.soundtrack.as_dict() == {"sample_rate": 8, "channels": 2, "sample_frames": 8}
            assert result.as_dict()["fmtl_minor"] == 2
            assert result.camera_track == camera and not result.as_dict()["certified_source"]
            assert scene.construct_calls == 1
            assert [row[1:3] for row in scene._sound_request_facts()] == [(0, 8), (2, 8)]
            rate, channels, pcm = _test_bundle_pcm(data)
            assert rate == 8 and channels == 2
            assert len(pcm) == len(expected)
            assert max(abs(a - b) for a, b in zip(pcm, expected)) < 1e-7
            updates = scene.updates
            assert replay(data, list(range(4)), 64, 36, workers) == pictures
            assert replay(data, [3, 0, 2, 1], 64, 36, workers) == [pictures[i] for i in (3, 0, 2, 1)]
            assert scene.updates == updates
            assert str(background).encode() not in data and str(lead).encode() not in data
            if reference is None:
                reference = data
            assert reference == data
        exact = export_bundle(Selected, root / f"exact-{camera}.fmtl", audio=True, camera=camera,
                              audio_sample_rate=8, resolution=(64, 36), fps=8, max_output_bytes=len(reference))
        assert exact.bytes == len(reference)
        limited = root / f"limited-{camera}.fmtl"
        try:
            export_bundle(Selected, limited, audio=True, camera=camera, audio_sample_rate=8,
                          resolution=(64, 36), fps=8, max_output_bytes=len(reference) - 1)
        except RuntimeError:
            pass
        else:
            raise AssertionError("combined picture/audio byte budget ignored")
        assert not limited.exists()

    # Native resampling must match the existing direct WAV route, not just our
    # small analytic sample fixture. A nonexistent ffmpeg path cannot affect WAV.
    class NoExternalDecoder(Narrated):
        def construct(self):
            self.file_writer.ffmpeg_bin = "/no-ffmpeg-for-native-wav"
            super().construct()
    rendered = root / "direct.wav"
    render_scene(NoExternalDecoder, rendered, format="wav", fps=8, resolution=(64, 36), threads=1)
    audible = root / "resampled.fmtl"
    result = export_bundle(NoExternalDecoder, audible, audio=True, fps=8, resolution=(64, 36))
    rate, channels, pcm = _test_bundle_pcm(audible.read_bytes())
    direct = _test_wav_pcm(str(rendered))
    assert (rate, channels) == (48000, 2) and len(pcm) == len(direct)
    assert max(abs(a - b) for a, b in zip(pcm, direct)) <= 1 / 32768

    class Short(m.Scene):
        hold = False
        def construct(self):
            self.add(m.Square())
            self.add_sound(str(short))
            if self.hold:
                self.wait(0.5)
    for hold in (False, True):
        class Held(Short):
            pass
        Held.hold = hold
        dest = root / f"short-{hold}.fmtl"
        receipt = export_bundle(Held, dest, audio=True, audio_sample_rate=8, fps=8)
        assert receipt.frame_count == (4 if hold else 1)
        samples = _test_bundle_pcm(dest.read_bytes())[2]
        assert samples == [0.5, 0.5] + ([0.0] * 6 if hold else [])

    class Negative(m.Scene):
        def construct(self):
            self.add_sound(str(lead), time_offset=-0.125)
            self.wait(0.25)
    negative = root / "negative.fmtl"
    export_bundle(Negative, negative, audio=True, audio_sample_rate=8, fps=8)
    assert _test_bundle_pcm(negative.read_bytes())[2] == [0.5, -0.5, 0.0, 0.0]

    # Repeated source paths consume raw/decode budget once, but each prepared
    # cue is charged. This exact cap cannot admit two independent source reads.
    class Repeated(m.Scene):
        def construct(self):
            self.add_sound(str(lead))
            self.wait(0.375)
            self.add_sound(str(lead))
            self.wait(0.25)
    repeated = root / "repeated.fmtl"
    receipt = export_bundle(Repeated, repeated, audio=True, audio_sample_rate=8, fps=8, max_audio_bytes=64)
    assert _test_bundle_pcm(repeated.read_bytes())[2] == [0.25, -0.25, 0.5, -0.5, 0.0, 0.0, 0.25, -0.25, 0.5, -0.5]
    try:
        export_bundle(Repeated, root / "prepared-over.fmtl", audio=True, audio_sample_rate=8, fps=8, max_audio_bytes=63)
    except RuntimeError as error:
        assert "prepared-sample budget" in str(error)
    else:
        raise AssertionError("repeated prepared cues were not charged separately")
    assert not (root / "prepared-over.fmtl").exists()

    class ExplicitStill(m.Scene):
        def construct(self):
            self.add(m.Square())
            self.show()
            self.add_sound(str(short))
    explicit = root / "explicit-show.fmtl"
    try:
        export_bundle(ExplicitStill, explicit, audio=True, audio_sample_rate=8, fps=8)
    except (m._CapabilityError, RuntimeError) as error:
        assert "show" in str(error)
    else:
        raise AssertionError("an explicit hold silently changed cue timing")
    assert not explicit.exists()

    pristine = m.Scene()
    try:
        m._portal_begin_soundtrack_bundle(pristine, str(root / "invalid.fmtl"), 64, 36, 8, 0,
                                         100, 65536, 65536, False, 0, 2, 1, 65536)
    except ValueError:
        pass
    else:
        raise AssertionError("native audio policy accepted a zero sample rate")
    with BundleExportSession(pristine, root / "pristine.fmtl", audio=True, audio_sample_rate=8, fps=8):
        pristine.add_sound(str(short))

    # Wrong finish entry cannot consume or cancel the live owner.
    for audio in (False, True):
        dest = root / f"owner-{audio}.fmtl"
        scene = m.Scene()
        with BundleExportSession(scene, dest, audio=audio, fps=8, audio_sample_rate=8 if audio else 48000) as session:
            wrong = m._portal_finish_bundle if audio else m._portal_finish_soundtrack_bundle
            try:
                wrong(scene)
            except RuntimeError as error:
                assert "finish mode" in str(error)
            else:
                raise AssertionError("mismatched native finish consumed its owner")
            scene.add(m.Square())
            if audio:
                scene.add_sound(str(short))
        assert session.artifact_published and dest.exists()

    # Every failure must leave no successful partial output and release ownership.
    for fault in ("missing", "malformed", "compressed", "budget", "capture"):
        dest = root / f"failed-{fault}.fmtl"
        source = root / f"{fault}.wav"
        if fault == "malformed":
            source.write_bytes(b"not a wave")
        if fault == "compressed":
            source.write_bytes(b"fLaC" + bytes(40))
        class Broken(m.Scene):
            def construct(self):
                self.add_sound(str(source if fault in ("missing", "malformed", "compressed") else background))
                try:
                    self.wait(0.5)
                except RuntimeError:
                    if fault != "capture":
                        raise
        try:
            export_bundle(Broken, dest, audio=True, audio_sample_rate=8, fps=8,
                          max_audio_bytes=16 if fault == "budget" else 65536,
                          max_frames=1 if fault == "capture" else 100)
        except (OSError, ValueError, RuntimeError):
            pass
        else:
            raise AssertionError(f"failed {fault} export published")
        assert not dest.exists()

    dest = root / "raced.fmtl"
    with_ownership = m.Scene()
    try:
        with BundleExportSession(with_ownership, dest, audio=True, audio_sample_rate=8, fps=8):
            with_ownership.add_sound(str(short))
            dest.write_bytes(b"concurrent winner")
    except FileExistsError:
        pass
    else:
        raise AssertionError("soundtrack publication replaced a concurrent output")
    assert dest.read_bytes() == b"concurrent winner"
    background.unlink()
    lead.unlink()
    # Original sources are irrelevant to portable replay.
    assert _test_bundle_pcm(audible.read_bytes())[2] == pcm
    assert len(_test_bundle_frames(audible.read_bytes(), [0, 3], 64, 36, 4)) == 2
    print("Python soundtrack acceptance: exact PCM, camera/planar pixels, resampling, timing, budgets and ownership PASS")
