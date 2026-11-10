"""Typed FMTL publication receipts for the existing batch checkpoint journal.

This is not an FMTL reader, source cache, or second persistence format. The
native exporter validates bundle bytes before publication; BatchCheckpoint
independently rehashes those files before trusting a completed publication.
"""
import copy
from pathlib import Path

from .bundle_export import BundleExportResult, BundleSoundtrackInfo, _CAP

_REQUIRED = frozenset({"schema", "version", "format", "destination", "frame_count",
                       "segment_count", "bytes", "sha256", "fps", "replay_resolution",
                       "certified_source"})
_CAMERA = frozenset({"camera_track", "fmtl_minor"})
_SOUNDTRACK = frozenset({"soundtrack", "fmtl_minor"})

_PROVENANCE = frozenset({"audio_inputs", "ffmpeg_invocations", "ffmpeg_limits"})


def _restore_audio_provenance(data, options):
    """Restore observations only; never open sources or replay decoder argv."""
    inputs, invocations, limits = (data[name] for name in
                                  ("audio_inputs", "ffmpeg_invocations", "ffmpeg_limits"))

    def integer(value, minimum=0, maximum=(1 << 64) - 1):
        return type(value) is int and minimum <= value <= maximum

    def text(value, *, path=False):
        return isinstance(value, str) and bool(value) and (not path or "\0" not in value)

    def digest(value):
        return (isinstance(value, str) and len(value) == 64
                and all(char in "0123456789abcdef" for char in value))

    if (not isinstance(inputs, list) or not 1 <= len(inputs) <= 4096
            or not isinstance(invocations, list) or len(invocations) > len(inputs)):
        raise ValueError("invalid checkpoint bundle audio provenance lists")
    expected_limits = {"timeout_seconds", "timeout_nanoseconds", "max_log_bytes",
                       "max_artifact_bytes", "max_input_bytes", "keep_workdir"}
    if (not isinstance(limits, dict) or set(limits) != expected_limits
            or not integer(limits["timeout_seconds"])
            or not integer(limits["timeout_nanoseconds"], 0, 999_999_999)
            or not (limits["timeout_seconds"] or limits["timeout_nanoseconds"])
            or any(not integer(limits[name], 1) for name in
                   ("max_log_bytes", "max_artifact_bytes", "max_input_bytes"))
            or type(limits["keep_workdir"]) is not bool):
        raise ValueError("invalid checkpoint bundle decoder limits")
    expected_input = {"path", "source_sha256", "pcm_sha256", "container",
                      "sample_rate", "channels", "sample_frames", "decoder", "decoder_sha256"}
    sources, external_sources, tool_hashes = {}, set(), set()
    for item in inputs:
        if (not isinstance(item, dict) or set(item) != expected_input
                or not text(item["path"], path=True)
                or not digest(item["source_sha256"]) or not digest(item["pcm_sha256"])
                or item["container"] not in ("wav", "flac", "mp3", "aac", "ogg", "mov", "aiff")
                or not integer(item["sample_rate"], 1, (1 << 32) - 1)
                or not integer(item["channels"], 1, 2)
                or not integer(item["sample_frames"])):
            raise ValueError("invalid checkpoint bundle audio input")
        if item["decoder"] == "native-wav":
            if item["container"] != "wav" or item["decoder_sha256"] is not None:
                raise ValueError("invalid checkpoint native WAV decoder identity")
        elif item["decoder"] == "ffmpeg" and digest(item["decoder_sha256"]):
            external_sources.add(item["path"])
            tool_hashes.add(item["decoder_sha256"])
        else:
            raise ValueError("invalid checkpoint bundle audio decoder")
        previous = sources.setdefault(item["path"], item)
        if previous != item:
            raise ValueError("checkpoint repeated audio source has inconsistent observations")
        if (options is not None and item["sample_frames"] * item["channels"] * 4
                > options["bundle_audio_options"]["max_audio_bytes"]):
            raise ValueError("checkpoint decoded source exceeds the admitted audio budget")
    expected_invocation = {"tool_path", "tool_sha256", "tool_version", "bound_tool_path",
                           "process_mechanism", "process_policy_version", "native_image",
                           "encoder", "hardware_encoder_probe", "argv"}
    image_fields = {"format", "architecture", "file_bytes", "policy_version"}
    identity, invocation_hashes = None, set()
    for item in invocations:
        if (not isinstance(item, dict) or set(item) != expected_invocation
                or not text(item["tool_path"], path=True) or not text(item["bound_tool_path"], path=True)
                or not digest(item["tool_sha256"]) or not text(item["tool_version"])
                or item["process_mechanism"] not in (
                    "posix_spawn.absolute_path.new_process_group",
                    "create_process_w.explicit_application.atomic_job_list", "scripted.process_runner")
                or not integer(item["process_policy_version"], 1, (1 << 32) - 1)
                or item["encoder"] != "pcm_f32le" or item["hardware_encoder_probe"] is not None
                or not isinstance(item["argv"], list) or not 1 <= len(item["argv"]) <= 256
                or any(not isinstance(value, str) or "\0" in value for value in item["argv"])):
            raise ValueError("invalid checkpoint bundle decoder invocation")
        image = item["native_image"]
        if (not isinstance(image, dict) or set(image) != image_fields
                or image["format"] not in ("elf64", "macho64", "macho-universal", "pe32plus")
                or image["architecture"] not in ("x86_64", "aarch64")
                or not integer(image["file_bytes"], 1)
                or not integer(image["policy_version"], 1, (1 << 32) - 1)):
            raise ValueError("invalid checkpoint bundle decoder image")
        observed = tuple(item[name] for name in
                         ("tool_path", "tool_sha256", "tool_version", "process_mechanism",
                          "process_policy_version", "native_image"))
        if identity is not None and observed != identity:
            raise ValueError("checkpoint bundle contains multiple decoder identities")
        identity = observed
        invocation_hashes.add(item["tool_sha256"])
    # PathBuf cache keys can normalize dots/separators that original path
    # strings retain. Receipts have no per-allocation ID, so exact decode counts
    # or cumulative cached PCM cannot be reconstructed by summing path strings.
    if (invocation_hashes != tool_hashes or len(tool_hashes) > 1
            or len(invocations) > len(external_sources)):
        raise ValueError("checkpoint audio inputs and decoder invocations disagree")
    return tuple(copy.deepcopy(inputs)), tuple(copy.deepcopy(invocations)), copy.deepcopy(limits)



def _restore_soundtrack(data, artifact_bytes):
    if not isinstance(data, dict) or set(data) != {"sample_rate", "channels", "sample_frames"}:
        raise ValueError("invalid checkpoint bundle soundtrack fields")
    for field, minimum, maximum in (("sample_rate", 1, 384_000), ("channels", 1, 2),
                                     ("sample_frames", 0, _CAP // 4)):
        if type(data[field]) is not int or not minimum <= data[field] <= maximum:
            raise ValueError("invalid checkpoint bundle soundtrack field: " + field)
    if data["sample_frames"] * data["channels"] * 4 > artifact_bytes:
        raise ValueError("checkpoint soundtrack PCM exceeds the bundle artifact")
    return BundleSoundtrackInfo(data["sample_rate"], data["channels"], data["sample_frames"])


def restore_bundle_receipt(data, *, destination=None, options=None):
    """Validate native receipt shape and admitted settings before reusing it.

    The key/plan and output inventory are checked by the journal owner. No
    field in a checkpoint can turn a captured Python picture into certified
    source execution, or change its camera/audio/storage mode. Audio-enabled scenes
    with no cues legitimately retain the legacy picture-only receipt.
    """
    if not isinstance(data, dict):
        raise ValueError("invalid checkpoint bundle receipt")
    camera, audio, compact = "camera_track" in data, "soundtrack" in data, "storage" in data
    provenance = bool(_PROVENANCE.intersection(data))
    expected = _REQUIRED
    if provenance:
        expected |= _PROVENANCE
        if not audio:
            raise ValueError("checkpoint audio provenance requires a soundtrack")
    if camera:
        expected |= _CAMERA
    if audio:
        expected |= _SOUNDTRACK
    if compact:
        if data["storage"] != "compact":
            raise ValueError("invalid checkpoint bundle storage metadata")
        expected |= {"storage", "fmtl_minor"}
        if audio:
            expected |= {"picture_fmtl_minor"}
    if frozenset(data) != expected:
        raise ValueError("invalid checkpoint bundle receipt fields")
    if (data["schema"] != "fmn.python-bundle-export" or type(data["version"]) is not int
            or data["version"] != 1 or data["format"] != "fmtl"
            or data["certified_source"] is not False):
        raise ValueError("invalid checkpoint bundle receipt schema or certification")
    if camera and data["camera_track"] is not True:
        raise ValueError("invalid checkpoint bundle camera metadata")
    if camera or audio or compact:
        minor = 2 if audio else 3 if compact else 1
        if type(data["fmtl_minor"]) is not int or data["fmtl_minor"] != minor:
            raise ValueError("invalid checkpoint bundle camera/audio/storage version")
    if compact and audio:
        if type(data["picture_fmtl_minor"]) is not int or data["picture_fmtl_minor"] != 3:
            raise ValueError("invalid checkpoint compact soundtrack picture version")
    name = data["destination"]
    if (not isinstance(name, str) or not name or "\0" in name
            or not Path(name).is_absolute() or ".." in Path(name).parts):
        raise ValueError("invalid checkpoint bundle destination")
    path = Path(name)
    if destination is not None and path != destination:
        raise ValueError("checkpoint bundle destination differs from the scene plan")
    for field, maximum in (("frame_count", (1 << 32) - 1),
                           ("segment_count", (1 << 64) - 1), ("bytes", _CAP), ("fps", 240)):
        if type(data[field]) is not int or not 1 <= data[field] <= maximum:
            raise ValueError("invalid checkpoint bundle receipt field: " + field)
    resolution = data["replay_resolution"]
    if (not isinstance(resolution, list) or len(resolution) != 2
            or any(type(value) is not int or value <= 0 for value in resolution)
            or resolution[0] * resolution[1] > 16_777_216):
        raise ValueError("invalid checkpoint bundle resolution")
    digest = data["sha256"]
    if (not isinstance(digest, str) or len(digest) != 64
            or any(char not in "0123456789abcdef" for char in digest)):
        raise ValueError("invalid checkpoint bundle digest")
    soundtrack = _restore_soundtrack(data["soundtrack"], data["bytes"]) if audio else None
    if options is not None:
        permitted = options.get("bundle_audio_options", {}).get("audio_ffmpeg", False)
        if provenance and permitted is not True:
            raise ValueError("checkpoint decoder provenance requires the admitted audio capability")
        if audio and permitted is True and not provenance:
            raise ValueError("checkpoint soundtrack is missing its decoder provenance")
    audio_inputs, invocations, decoder_limits = (
        _restore_audio_provenance(data, options) if provenance else ((), (), None))
    if options is not None:
        storage = "compact" if compact else "recorded"
        if storage != options.get("bundle_storage", "recorded"):
            raise ValueError("checkpoint bundle storage mode differs from the scene plan")
        if camera != options["bundle_camera"]:
            raise ValueError("checkpoint bundle camera mode differs from the scene plan")
        if audio:
            if options.get("bundle_audio") is not True:
                raise ValueError("checkpoint soundtrack requires an audio-enabled scene plan")
            audio_options = options["bundle_audio_options"]
            if (soundtrack.sample_rate != audio_options["audio_sample_rate"]
                    or soundtrack.channels != audio_options["audio_channels"]):
                raise ValueError("checkpoint soundtrack layout differs from the scene plan")
            if soundtrack.sample_frames * soundtrack.channels * 4 > audio_options["max_audio_bytes"]:
                raise ValueError("checkpoint soundtrack exceeds the admitted audio budget")
        if options["fps"] is not None and data["fps"] != options["fps"]:
            raise ValueError("checkpoint bundle FPS differs from the scene plan")
        if options["resolution"] is not None and list(options["resolution"]) != resolution:
            raise ValueError("checkpoint bundle resolution differs from the scene plan")
        limits = options["bundle_limits"]
        if data["frame_count"] > limits["max_frames"] or data["bytes"] > limits["max_output_bytes"]:
            raise ValueError("checkpoint bundle exceeds the admitted recording limits")
    return BundleExportResult(path, data["frame_count"], data["segment_count"],
                              data["bytes"], digest, data["fps"], tuple(resolution), camera, soundtrack,
                              "compact" if compact else "recorded",
                              audio_inputs, invocations, decoder_limits)
