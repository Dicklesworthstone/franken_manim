"""FMTL admission for source-file workers; capture and receipts stay shared.

Only plain settings cross process boundaries. The native recorder, soundtrack
mixer, publication owner and checkpoint receipt schema are not reimplemented.
"""
from collections.abc import Mapping


def prepare_bundle_options(native, *, camera, audio, limits, audio_options,
                           resolution, fps, threads, animation_range, subdivide,
                           save_last_frame, reproducible, output_options):
    from .bundle_export import (
        _validated_audio_options, _validated_limits, require_bundle_capability,
    )

    if type(camera) is not bool or type(audio) is not bool:
        raise TypeError("bundle_camera and bundle_audio must be bool")
    if (threads is not None or animation_range is not None or subdivide
            or save_last_frame or reproducible or output_options):
        raise native._CapabilityError(
            "FMTL process batches require complete scene capture without pixel-output "
            "options, threads, ranges, subdivision, paired output or source certification"
        )
    if ((resolution is not None and resolution[0] * resolution[1] > 16_777_216)
            or (fps is not None and fps > 240)):
        raise ValueError("bundle export requires at most 16M pixels and 1..240 FPS")
    if limits is not None and not isinstance(limits, Mapping):
        raise TypeError("bundle_limits must be a mapping or None")
    if audio_options is not None and not isinstance(audio_options, Mapping):
        raise TypeError("bundle_audio_options must be a mapping or None")
    if audio_options is not None and not audio:
        raise ValueError("bundle_audio_options require bundle_audio=True")
    result = {"bundle_camera": camera,
              "bundle_limits": _validated_limits(**({} if limits is None else dict(limits)))}
    if audio:
        rate, channels, workers, budget = _validated_audio_options(
            audio=True, **({} if audio_options is None else dict(audio_options)))
        result.update(bundle_audio=True, bundle_audio_options={
            "audio_sample_rate": rate, "audio_channels": channels,
            "audio_threads": workers, "max_audio_bytes": budget,
        })
    require_bundle_capability(native, camera=camera, audio=audio)
    return result
