"""Native clip composition through the public Python facade and a real child.

The supplied executable is the Rust concat-ordered fixture: it checks private
input bytes and argv, then emits a marker. It is not an encoded-media decoder.
"""
import hashlib
from pathlib import Path
import shutil

import manimlib as m
from fmn_python import concatenate_video
from fmn_python import effect_audit


def run_video_composition_acceptance(executable, directory):
    root = Path(directory)
    root.mkdir(parents=True, exist_ok=True)
    (root / ".fmn-native-ffmpeg-mode").write_text("concat-ordered")
    fixture = root / "concat-ordered"
    shutil.copy2(executable, fixture)
    sources = (root / "A.mp4", root / "it's\nB.mov")
    payloads = (b"fmn-concat-clip-A", b"fmn-concat-clip-B")
    for path, data in zip(sources, payloads):
        path.write_bytes(data)
    inputs = (sources[0], sources[1], sources[0])
    total = len(payloads[0]) * 2 + len(payloads[1])
    result = concatenate_video(
        iter(inputs), root / "joined.mp4", ffmpeg_bin=fixture, workdir=root,
        max_clips=3, max_clip_bytes=len(payloads[0]), max_input_bytes=total,
        timeout=10, max_output_bytes=1024,
    )
    marker = b"FMN_CONCAT_ORDER:A,B,A\n"
    assert result.destination.read_bytes() == marker
    assert result.bytes == len(marker)
    assert result.digest == hashlib.sha256(marker).hexdigest()
    assert result.total_input_bytes == total
    assert tuple(entry.source for entry in result.inputs) == inputs
    for entry, data in zip(result.inputs, (payloads[0], payloads[1], payloads[0])):
        assert (entry.bytes, entry.digest) == (len(data), hashlib.sha256(data).hexdigest())
    assert result.source_limits == {
        "max_clips": 3, "max_clip_bytes": len(payloads[0]), "max_input_bytes": total,
    }
    assert result.ffmpeg_limits["timeout_seconds"] == 10
    assert result.ffmpeg_limits["max_input_bytes"] == total
    assert result.ffmpeg_limits["max_artifact_bytes"] == 1024
    assert not result.certified
    assert len(result.ffmpeg_invocations) == 1
    invocation = result.ffmpeg_invocations[0]
    assert Path(invocation["tool_path"]) == fixture.resolve()
    assert invocation["tool_sha256"] == hashlib.sha256(fixture.read_bytes()).hexdigest()
    argv = invocation["argv"]
    assert argv[argv.index("-map") + 1] == "0"
    assert argv[argv.index("-c") + 1] == "copy"
    assert not Path(argv[argv.index("-i") + 1]).parent.exists(), "private input files leaked"
    assert not Path(invocation["bound_tool_path"]).exists(), "private executable leaked"

    def rejected(error_type, clips, target, **options):
        options.setdefault("ffmpeg_bin", root / "missing-ffmpeg")
        try:
            concatenate_video(clips, target, **options)
        except error_type as error:
            return str(error)
        raise AssertionError("invalid native composition was admitted")

    # These failures must precede discovering the deliberately missing tool.
    assert "already exists" in rejected(FileExistsError, inputs, result.destination)
    assert result.destination.read_bytes() == marker
    assert "limit" in rejected(ValueError, inputs, root / "too-many-bytes.mp4",
                               max_input_bytes=total - 1)
    assert not (root / "too-many-bytes.mp4").exists()
    assert "input 0" in rejected(FileNotFoundError, [root / "missing.mp4"], root / "absent.mp4")
    empty = root / "empty.mp4"
    empty.write_bytes(b"")
    assert "empty" in rejected(ValueError, [empty], root / "empty-result.mp4")
    assert "native outputs" in rejected(m._CapabilityError, inputs, root / "no-tool.mp4")

    # A caller cannot bypass the native bound through the private host entry.
    policy = dict(max_clips=3, max_clip_bytes=64, max_input_bytes=256,
                  timeout=10, max_log_bytes=1024, max_output_bytes=1024)
    for key, value in (("max_clips", True), ("max_input_bytes", 0)):
        invalid = dict(policy, **{key: value})
        try:
            m._portal_concatenate_video(tuple(map(str, inputs)), str(root / "invalid.mp4"),
                                       str(fixture), invalid)
        except (TypeError, ValueError):
            pass
        else:
            raise AssertionError("private boundary admitted invalid limits")

    recorder = effect_audit.begin()
    try:
        message = rejected(m._CapabilityError, inputs, root / "certified.mp4", ffmpeg_bin=fixture)
        assert "outside the certified artifact set" in message
    finally:
        recorder.stop()
    assert not (root / "certified.mp4").exists()
    return len(result.inputs)
