"""Real Python scenes -> native mixer -> self-contained FMTL; no mocked engine."""
from pathlib import Path
import hashlib
import os
import runpy
import shutil
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



def write_float_wav(path, channels, rate, payload):
    fmt = struct.pack("<HHIIHH", 3, channels, rate, rate * channels * 4, channels * 4, 32)
    path.write_bytes(b"RIFF" + struct.pack("<I", 36 + len(payload)) + b"WAVEfmt " +
                     struct.pack("<I", 16) + fmt + b"data" + struct.pack("<I", len(payload)) + payload)


def verify_opted_soundtrack_exports(root, native_source):
    """Count completed artifact comparisons, keeping external coverage explicit."""
    global _shared_bundle_receipt_scene
    verified, compressed = 0, 0
    missing = str(root / "no installed decoder")

    class Silent(m.Scene):
        def construct(self):
            self.construct_calls = getattr(self, "construct_calls", 0) + 1
            self.add(m.Square())
            self.wait(0.25)

    for storage in ("recorded", "compact"):
        for camera in (False, True):
            options = dict(storage=storage, camera=camera, resolution=(64, 36), fps=8)
            plain = root / f"opt-silent-{storage}-{camera}.fmtl"
            offered = root / f"opt-silent-offered-{storage}-{camera}.fmtl"
            export_bundle(Silent, plain, **options)
            result = export_bundle(Silent, offered, audio=True, audio_ffmpeg=True,
                                   _output_options={"ffmpeg_bin": missing}, **options)
            assert plain.read_bytes() == offered.read_bytes()
            assert result.soundtrack is None
            assert result.audio_inputs == result.ffmpeg_invocations == ()
            assert result.ffmpeg_limits is None
            assert not {"audio_inputs", "ffmpeg_invocations", "ffmpeg_limits"} & result.as_dict().keys()
            verified += 1

    def voiced(asset, camera=False):
        class Voiced(m.Scene):
            default_file_writer_config = {"ffmpeg_bin": missing}
            def construct(self):
                self.construct_calls = getattr(self, "construct_calls", 0) + 1
                # Constructor defaults and later live edits must not replace
                # the explicit console/API override captured for this owner.
                self.file_writer.ffmpeg_bin = missing
                self.file_writer.ffmpeg_timeout = 0
                self.add(m.Square(side_length=0.5, fill_opacity=1))
                self.add_sound(str(asset), gain=-3)
                self.wait(0.125)
                self.add_sound(str(asset), time_offset=-1 / 48000,
                               gain=-6, gain_to_background=-3)
                if camera:
                    self.camera.frame.shift(0.125 * m.RIGHT)
                self.wait(0.375)
        return Voiced

    expected_path = root / "opt-native-baseline.fmtl"
    native_path = root / "opt-native-offered.fmtl"
    export_bundle(voiced(native_source), expected_path, audio=True, fps=8, resolution=(64, 36))
    _shared_bundle_receipt_scene = voiced(native_source)(
        file_writer_config={"ffmpeg_bin": missing, "ffmpeg_timeout": 37})
    result = export_bundle(_shared_bundle_receipt_scene, native_path, audio=True, audio_ffmpeg=True,
                           fps=8, resolution=(64, 36))
    assert native_path.read_bytes() == expected_path.read_bytes()
    assert len(result.audio_inputs) == 2 and result.ffmpeg_invocations == ()
    assert result.audio_inputs[0] == result.audio_inputs[1]
    assert result.audio_inputs[0]["decoder"] == "native-wav"
    assert result.audio_inputs[0]["decoder_sha256"] is None
    assert result.audio_inputs[0]["source_sha256"] == hashlib.sha256(native_source.read_bytes()).hexdigest()
    assert result.ffmpeg_limits["timeout_seconds"] == 37
    detached = result.as_dict()
    detached["audio_inputs"][0]["source_sha256"] = "mutated"
    detached["ffmpeg_limits"]["timeout_seconds"] = 1
    assert result.audio_inputs[0]["source_sha256"] != "mutated"
    assert result.ffmpeg_limits["timeout_seconds"] == 37
    verified += 1

    for index, (field, value) in enumerate((
            ("ffmpeg_bin", ""), ("ffmpeg_bin", "\0bad"), ("ffmpeg_timeout", 0),
            ("ffmpeg_timeout", True), ("ffmpeg_max_input_bytes", 0),
            ("ffmpeg_max_artifact_bytes", 0), ("ffmpeg_max_log_bytes", 0))):
        scene = None
        destination = root / f"opt-invalid-policy-{index}.fmtl"
        try:
            scene = Silent(file_writer_config={field: value})
            export_bundle(scene, destination, audio=True, audio_ffmpeg=True, fps=8)
        except (TypeError, ValueError):
            pass
        else:
            raise AssertionError(f"invalid decoder policy {field} was admitted")
        if scene is not None:
            assert getattr(scene, "construct_calls", 0) == 0
            assert "_fmn_owned_render_session" not in vars(scene)
        assert not destination.exists()

    descriptor_scene = m.Scene()
    class MutatingWriter:
        @property
        def ffmpeg_bin(self):
            descriptor_scene.add_sound(str(native_source))
            return missing
    descriptor_scene.file_writer = MutatingWriter()
    try:
        with BundleExportSession(descriptor_scene, root / "opt-descriptor.fmtl",
                                 audio=True, audio_ffmpeg=True):
            raise AssertionError("writer callback's native state was replaced")
    except RuntimeError as error:
        assert "mutates engine state" in str(error), error
    assert len(descriptor_scene._sound_request_facts()) == 1
    assert not (root / "opt-descriptor.fmtl").exists()

    for name, payload, expected_error in (
            ("playlist", b"#EXTM3U\nhttps://example.invalid/voice.mp3", ValueError),
            ("damaged", native_source.read_bytes()[:-1], ValueError),
            ("missing-capability", b"fLaCbroken", m._CapabilityError)):
        asset = root / f"opt-{name}.asset"
        asset.write_bytes(payload)
        destination = root / f"opt-{name}.fmtl"
        try:
            export_bundle(voiced(asset), destination, audio=True, audio_ffmpeg=True,
                          _output_options={"ffmpeg_bin": missing}, fps=8)
        except expected_error:
            pass
        else:
            raise AssertionError(f"{name} silently published a soundtrack")
        assert not destination.exists()

    ffmpeg = os.environ.get("FMN_AUDIO_FFMPEG") or shutil.which("ffmpeg")
    if not ffmpeg:
        assert not os.environ.get("FMN_REQUIRE_FFMPEG"), "required compressed bundle decoder is unavailable"
        print("bundle audio: native opt-in and refusals verified; external decode unavailable")
        return verified, compressed

    fixtures = root / "compressed fixtures"
    generator = Path(__file__).resolve().parents[2] / "fmn-output/tests/make_audio_fixtures.py"
    runpy.run_path(str(generator))["make"](fixtures)
    explicit = root / "decoder with spaces"
    shutil.copyfile(ffmpeg, explicit)
    explicit.chmod(0o700)
    tool_hash = hashlib.sha256(explicit.read_bytes()).hexdigest()
    rows = [line.split("\t") for line in (fixtures / "manifest.tsv").read_text().splitlines()]
    assert len(rows) == 8, "compressed format coverage changed"
    references = {}
    for asset_name, reference_name, source_channels, source_rate in rows:
        asset = fixtures / asset_name
        payload = (fixtures / reference_name).read_bytes()
        source_channels, source_rate = int(source_channels), int(source_rate)
        reference = root / f"{asset_name}.native.wav"
        write_float_wav(reference, source_channels, source_rate, payload)
        references[asset_name] = reference
        for storage in ("recorded", "compact"):
            for camera in (False, True):
                options = dict(audio=True, storage=storage, camera=camera,
                               fps=8, resolution=(64, 36), audio_sample_rate=48000)
                baseline = root / f"{asset_name}-{storage}-{camera}-native.fmtl"
                destination = root / f"{asset_name}-{storage}-{camera}-compressed.fmtl"
                export_bundle(voiced(reference, camera), baseline, audio_threads=1, **options)
                scene = voiced(asset, camera)(file_writer_config={"ffmpeg_timeout": 37})
                result = export_bundle(scene, destination, audio_ffmpeg=True, audio_threads=4,
                                       _output_options={"ffmpeg_bin": str(explicit)}, **options)
                assert destination.read_bytes() == baseline.read_bytes(), (asset_name, storage, camera)
                assert scene.construct_calls == 1 and result.frame_count == 4
                assert len(result.audio_inputs) == 2 and len(result.ffmpeg_invocations) == 1
                assert result.audio_inputs[0] == result.audio_inputs[1]
                fact = result.audio_inputs[0]
                assert fact["decoder"] == "ffmpeg" and fact["decoder_sha256"] == tool_hash
                assert fact["source_sha256"] == hashlib.sha256(asset.read_bytes()).hexdigest()
                assert (fact["channels"], fact["sample_rate"], fact["sample_frames"]) == (
                    source_channels, source_rate, len(payload) // 4 // source_channels)
                assert result.ffmpeg_limits["timeout_seconds"] == 37
                invocation = result.ffmpeg_invocations[0]
                assert Path(invocation["tool_path"]).samefile(explicit)
                assert invocation["tool_sha256"] == tool_hash
                assert invocation["encoder"] == "pcm_f32le"
                assert str(asset) not in invocation["argv"]
                assert "-ar" not in invocation["argv"] and "-ac" not in invocation["argv"]
                replay = _test_camera_bundle_frames if camera else _test_bundle_frames
                assert replay(destination.read_bytes(), [0, 3], 64, 36, 4) == replay(
                    baseline.read_bytes(), [0, 3], 64, 36, 1)
                assert _test_bundle_pcm(destination.read_bytes()) == _test_bundle_pcm(baseline.read_bytes())
                verified += 1
                compressed += 1

    # One retained tool handles distinct source layouts; a repeated source
    # still produces cue-ordered facts without another decoder invocation.
    def multiple(first, second):
        class Multiple(m.Scene):
            def construct(self):
                for asset in (first, second, first):
                    self.add_sound(str(asset))
                    self.wait(0.125)
        return Multiple
    first, second = fixtures / "flac.asset", fixtures / "mono.asset"
    baseline, destination = root / "opt-multiple-native.fmtl", root / "opt-multiple-compressed.fmtl"
    export_bundle(multiple(references["flac.asset"], references["mono.asset"]),
                  baseline, audio=True, fps=8)
    result = export_bundle(multiple(first, second), destination, audio=True, audio_ffmpeg=True,
                           _output_options={"ffmpeg_bin": str(explicit)}, fps=8)
    assert destination.read_bytes() == baseline.read_bytes()
    assert len(result.audio_inputs) == 3 and len(result.ffmpeg_invocations) == 2
    assert result.audio_inputs[0] == result.audio_inputs[2]
    verified += 1
    compressed += 1

    # Host cwd/PATH edits after generation acquisition cannot redirect the
    # explicit relative executable path or its admitted process policy.
    previous_cwd, previous_path = Path.cwd(), os.environ.get("PATH")
    elsewhere = root / "changed host context"
    elsewhere.mkdir()
    class ChangedContext(m.Scene):
        def construct(self):
            self.file_writer.ffmpeg_bin = missing
            os.chdir(elsewhere)
            os.environ["PATH"] = str(elsewhere)
            self.add_sound(str(first))
            self.wait(0.125)
    try:
        os.chdir(root)
        result = export_bundle(ChangedContext, root / "opt-frozen-host.fmtl", audio=True,
                               audio_ffmpeg=True, fps=8,
                               _output_options={"ffmpeg_bin": "./decoder with spaces"})
    finally:
        os.chdir(previous_cwd)
        if previous_path is None:
            os.environ.pop("PATH", None)
        else:
            os.environ["PATH"] = previous_path
    assert result.audio_inputs[0]["decoder_sha256"] == tool_hash
    assert len(result.ffmpeg_invocations) == 1
    verified += 1
    compressed += 1

    for name, settings, allowed in (
            ("default-native-only", {}, False),
            ("source-limit", {"ffmpeg_max_input_bytes": 1}, True),
            ("expansion-limit", {"ffmpeg_max_artifact_bytes": 1}, True)):
        destination = root / f"opt-refused-{name}.fmtl"
        overrides = {"_output_options": {"ffmpeg_bin": str(explicit)}} if allowed else {}
        try:
            export_bundle(voiced(first), destination, audio=True, audio_ffmpeg=allowed,
                          scene_kwargs={"file_writer_config": settings}, fps=8, **overrides)
        except (m._CapabilityError, RuntimeError, ValueError, OSError):
            pass
        else:
            raise AssertionError(f"{name} published despite its admission policy")
        assert not destination.exists()
    return verified, compressed


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

        compact_scene = Selected()
        compact_path = root / f"compact-sound-{camera}.fmtl"
        compact = export_bundle(compact_scene, compact_path, storage="compact", audio=True,
                                camera=camera, audio_sample_rate=8, audio_threads=4,
                                resolution=(64, 36), fps=8)
        compact_data = compact_path.read_bytes()
        assert compact.as_dict()["storage"] == "compact"
        assert compact.as_dict()["fmtl_minor"] == 2
        assert compact.as_dict()["picture_fmtl_minor"] == 3
        assert compact.camera_track == camera and compact_scene.construct_calls == 1
        assert _test_bundle_pcm(compact_data) == _test_bundle_pcm(reference)
        compact_updates = compact_scene.updates
        assert replay(compact_data, [3, 0, 2, 1], 64, 36, 4) == [pictures[i] for i in (3, 0, 2, 1)]
        assert compact_scene.updates == compact_updates
        compact_silent = root / f"compact-silent-{camera}.fmtl"
        silent_result = export_bundle(Silent, compact_silent, storage="compact", audio=True,
                                     camera=camera, resolution=(64, 36), fps=8)
        assert silent_result.as_dict()["fmtl_minor"] == 3
        assert silent_result.soundtrack is None
        assert _test_bundle_pcm(compact_silent.read_bytes()) is None

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
                                         100, 65536, 65536, False, 0, 2, 1, 65536, False)
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
    bundle_audio_verified_exports, bundle_audio_compressed_exports = verify_opted_soundtrack_exports(root, lead)
    background.unlink()
    lead.unlink()
    # Original sources are irrelevant to portable replay.
    assert _test_bundle_pcm(audible.read_bytes())[2] == pcm
    assert len(_test_bundle_frames(audible.read_bytes(), [0, 3], 64, 36, 4)) == 2
    print("Python soundtrack acceptance: exact PCM, camera/planar pixels, resampling, timing, budgets and ownership PASS")
