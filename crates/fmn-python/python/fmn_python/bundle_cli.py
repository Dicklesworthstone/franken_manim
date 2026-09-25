"""Console entry for Python Scene -> code-free FMTL/1 export.

The shared parser validates resolution/FPS and destination syntax. Its existing
PNG format is used only to parse those common options, never as an output sink
or fallback. The only output acquired here is BundleExportSession.
"""
from __future__ import annotations

import contextlib
import os
from pathlib import Path
import sys

from .bundle_export import BundleExportSession, require_bundle_capability
from .console_rendering import _tokens, _VALUE_FLAGS, _message
from .batch_rendering import _error_notes
from .scene_loading import SceneSource

BUNDLE_HELP = """Portable scene bundles:
  fmn-python [--robot] SOURCE.py [SCENE] --format fmtl
             [--resolution WIDTHxHEIGHT] [--fps FPS] [--video_dir FILE.fmtl]

Runs setup/construct/tear_down once in the host interpreter. Actual captured
frames become a bounded, code-free FMTL/1 artifact for standalone fmn/WASM.
--video_dir names the exact output file (default: media/videos/SOURCE/SCENE.fmtl).
Publication never overwrites a destination. FMTL/1 currently preserves planar
vectors/text at the default camera/light/background. Camera, audio, depth,
raster content, partial playback and source certification are explicit refusals.
Use the same --resolution when replaying; FMTL/1 stores FPS but not resolution.
"""

_ALLOWED = frozenset({"--format", "--resolution", "--fps", "--video_dir"})


def _bundle_options(arguments):
    """Recognize an actual format option, not 'fmtl' inside another value."""
    normalized, formats = [], []
    index = 0
    while index < len(arguments):
        token = arguments[index]
        if token in _VALUE_FLAGS:
            normalized.append(token)
            if index + 1 < len(arguments):
                value = arguments[index + 1]
                normalized.append(value)
                if token == "--format":
                    formats.append(value)
            index += 2
        elif token.startswith("--") and "=" in token:
            name, value = token.split("=", 1)
            if name in _VALUE_FLAGS:
                normalized.extend((name, value))
                if name == "--format":
                    formats.append(value)
            else:
                normalized.append(token)
            index += 1
        else:
            normalized.append(token)
            index += 1
    return normalized, formats


def try_bundle_cli(native, arguments):
    arguments, formats = _bundle_options(arguments)
    if "fmtl" not in formats:
        return None
    native_options, selectors, switches = _tokens(arguments, {"--robot"})
    robot = "--robot" in switches
    if "--help" in switches or "-h" in switches:
        return native._portal_cli_emit(0, "success", "help", BUNDLE_HELP, robot, help=BUNDLE_HELP)
    source = selected = destination = None
    session = None
    phase = "options"
    try:
        if len(formats) != 1 or switches.count("--robot") > 1:
            raise ValueError("bundle format and --robot must not be repeated")
        # Refuse unsupported semantics before importing or constructing source.
        # Unsupported modes cannot be silently ignored by a bundle export.
        index = 0
        while index < len(native_options):
            token = native_options[index]
            if token in _VALUE_FLAGS:
                if token not in _ALLOWED:
                    raise native._CapabilityError(f"{token} does not apply to FMTL export")
                index += 2
            else:
                raise native._CapabilityError(
                    f"{token} is not supported for full-scene FMTL export; use the PNG/video path"
                )
        require_bundle_capability(native)
        parser_options = list(native_options)
        parser_options[parser_options.index("--format") + 1] = "png_sequence"
        # Only common syntax/config validation is delegated. Never render PNG.
        positionals, values, width, height, fps, _threads = native._portal_cli_render_arguments(
            selectors + parser_options
        )
        if width * height > 16_777_216 or not 1 <= fps <= 240:
            raise ValueError("bundle export requires at most 16M pixels and 1..240 FPS")
        source = str(Path(positionals[0]).resolve())
        selected = positionals[1] if len(positionals) == 2 else None
        supplied = values["video_dir"]
        if supplied is not None:
            if not str(supplied) or "\0" in str(supplied):
                raise ValueError("bundle destination must be nonempty and contain no NUL")
            destination = Path(os.path.abspath(supplied))
        default_root = (Path("media") / "videos" / Path(source).stem).resolve()
        if destination is not None and os.path.lexists(destination):
            phase = "start"
            raise FileExistsError(f"bundle destination already exists: {destination}")
        phase = "load"
        redirect = contextlib.redirect_stdout(sys.stderr) if robot else contextlib.nullcontext()
        with redirect, SceneSource(source, native.Scene) as loaded:
            names = sorted(loaded.scenes)
            phase = "select"
            if selected is None:
                if len(names) != 1:
                    raise ValueError("select one Scene explicitly; discovered: " + (", ".join(names) or "none"))
                selected = names[0]
            if selected not in loaded.scenes:
                raise ValueError(f"scene {selected!r} was not declared by {source}")
            if destination is None:
                destination = default_root / (selected + ".fmtl")
            phase = "start"
            if os.path.lexists(destination):
                raise FileExistsError(f"bundle destination already exists: {destination}")
            phase = "construct"
            scene = loaded.scenes[selected]()
            phase = "start"
            session = BundleExportSession(scene, destination, resolution=(width, height), fps=fps, _native=native)
            with session:
                phase = "execute"
                try:
                    scene.run()
                except native.EndScene:
                    pass
                phase = "finish"
            if session.result is None:
                raise RuntimeError("bundle export returned without a publication receipt")
    except BaseException as error:
        if isinstance(error, KeyboardInterrupt):
            code, identity = 130, "interrupted"
        elif isinstance(error, native._CapabilityError):
            code, identity = 4, "capability"
        elif phase in {"options", "select"} and isinstance(error, (ValueError, TypeError)):
            code, identity = 2, "usage"
        elif phase in {"start", "finish"}:
            code, identity = 6, "render"
        else:
            code, identity = 5, "scene"
        return native._portal_cli_emit(
            code, identity, "bundle-export-failed", _message(error), robot,
            source=source, scene=selected, phase=phase, format="fmtl", exported=False,
            destination=None if destination is None else str(destination),
            artifact_published=session is not None and session.artifact_published,
            notes=list(_error_notes(error)),
        )
    receipt = session.result
    return native._portal_cli_emit(
        0, "success", "bundle-export", f"exported {receipt.frame_count} frames to {destination}", robot,
        source=source, scene=selected, format="fmtl", exported=True,
        destination=str(destination), frame_count=receipt.frame_count,
        bundle=receipt.as_dict(),
    )
