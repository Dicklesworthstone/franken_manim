"""Console rendering through the same generation owner as Scene.render.

The native parser remains the option authority; RenderSession owns cancellation
and publication. Authored source runs in the calling interpreter, with sibling
imports available and robot stdout reserved for the terminal receipt.
"""
from __future__ import annotations

import contextlib
from pathlib import Path
import sys
from typing import Any

from .batch_cli import _VALUE_FLAGS, _source_import_path, try_batch_cli
from .batch_rendering import _error_fields, _error_notes
from .rendering import RenderSession, _positive_integer

_CONTROL_FLAGS = frozenset({"--version", "--list-scenes", "--construct-only", "--audit-parity"})


def _tokens(arguments):
    """Separate positional selectors without interpreting native option values."""
    options, positionals, switches = [], [], []
    index = 0
    while index < len(arguments):
        value = arguments[index]
        if value in _VALUE_FLAGS:
            options.extend(arguments[index:index + 2])
            index += 2
            continue
        if value.startswith("-"):
            options.append(value)
            switches.append(value)
        else:
            positionals.append(value)
        index += 1
    return options, positionals, switches


def _message(error):
    name, message = _error_fields(error)
    return f"{name}: {message}"


def _single_result(native, result, robot, source, selected):
    details = result.as_dict()
    # Preserve the established CLI frame_count field for WAV consumers while
    # also exposing the unambiguous native sample-frame count.
    count = result.sample_frames if result.format == "wav" else result.frame_count
    details["frame_count"] = count
    unit = "sample frames" if result.format == "wav" else "frames"
    return native._portal_cli_emit(
        0, "success", "render",
        f"rendered {count} {result.format} {unit} to {result.destination}", robot,
        source=source, scene=selected, rendered=True, **details,
    )


def try_render_cli(native: Any, arguments: list[str]) -> int | None:
    """Own ordinary renders; leave non-render commands on their native route."""
    _, raw_positionals, switches = _tokens(arguments)
    if (_CONTROL_FLAGS.intersection(switches)
            or (raw_positionals and raw_positionals[0] == "studio")):
        return None
    batch = try_batch_cli(native, arguments)
    if batch is not None:
        return batch
    if not arguments or arguments == ["--robot"]:
        return None
    robot = switches.count("--robot") != 0
    if switches.count("--robot") > 1:
        return native._portal_cli_emit(2, "usage", "usage-error", "--robot must not be repeated", robot)
    # Only strip a real switch: an option value spelled --robot is data.
    remaining, index = [], 0
    while index < len(arguments):
        value = arguments[index]
        if value in _VALUE_FLAGS:
            remaining.extend(arguments[index:index + 2])
            index += 2
            continue
        if value != "--robot":
            remaining.append(value)
        index += 1
    try:
        positionals, options, width, height, fps, threads = native._portal_cli_render_arguments(remaining)
        for name, value in (("width", width), ("height", height), ("fps", fps), ("threads", threads)):
            _positive_integer(value, name)
        source = positionals[0]
        selected = positionals[1] if len(positionals) == 2 else None
        source_path = Path(source).resolve()
        supplied = options["video_dir"]
        if supplied is not None and (not str(supplied) or "\0" in str(supplied)):
            raise ValueError("render destination must be nonempty and contain no NUL")
        # Freeze both paths before module code or a constructor changes cwd.
        destination = None if supplied is None else Path(supplied).resolve()
        default_root = (Path("media") / "videos" / source_path.stem).resolve()
    except RuntimeError as error:
        message = _error_fields(error)[1]
        if message.startswith("CAPABILITY: "):
            return native._portal_cli_emit(4, "capability", "render-capability-unavailable",
                                           message.removeprefix("CAPABILITY: "), robot)
        return native._portal_cli_emit(2, "usage", "usage-error", message, robot)
    except (TypeError, ValueError, OSError) as error:
        return native._portal_cli_emit(2, "usage", "usage-error", _message(error), robot)

    phase, session = "load", None
    redirect = contextlib.redirect_stdout(sys.stderr) if robot else contextlib.nullcontext()
    try:
        with redirect, _source_import_path(source_path):
            scenes = native._portal_cli_scene_types(str(source_path))
            names = sorted(scenes)
            if selected is None:
                if len(names) != 1:
                    raise ValueError("select one scene explicitly; discovered: " + (", ".join(names) or "none"))
                selected = names[0]
            if selected not in scenes:
                raise ValueError(f"scene {selected!r} was not declared by {source}; discovered: "
                                 + (", ".join(names) or "none"))
            if destination is None:
                destination = (default_root / selected / "frames" if options["format"] == "png_sequence"
                               else default_root / (selected + "." + options["format"]))
            phase = "construct"
            scene = scenes[selected]()
            phase = "start"
            session = RenderSession(scene, destination, format=options["format"],
                                    resolution=(width, height), fps=fps, threads=threads, _native=native)
            with session:
                phase = "execute"
                try:
                    scene.run()
                except native.EndScene:
                    pass
                phase = "finish"
            if session.result is None:
                raise RuntimeError("scene execution ended without publishing its render generation")
    except (KeyboardInterrupt, SystemExit) as error:
        code = 130 if isinstance(error, KeyboardInterrupt) else 5
        return native._portal_cli_emit(code, "interrupted", "render-interrupted", _message(error), robot,
                                       source=source, scene=selected, phase=phase,
                                       destination=None if destination is None else str(destination),
                                       notes=list(_error_notes(error)),
                                       artifact_published=session is not None and session.result is not None)
    except Exception as error:
        capability = getattr(native, "_CapabilityError", ())
        if phase == "start" and isinstance(error, capability):
            code, identity, kind = 4, "capability", "render-capability-unavailable"
        elif phase in {"load", "construct"}:
            code, identity, kind = 5, "scene", "scene-load-failed"
        elif phase in {"start", "finish"}:
            code, identity, kind = 6, "render", "render-" + phase + "-failed"
        elif any(marker in _error_fields(error)[1] for marker in ("lumen:", "reel:", "portal-render:")):
            code, identity, kind = 6, "render", "render-failed"
        else:
            code, identity, kind = 5, "scene", "scene-execution-failed"
        return native._portal_cli_emit(code, identity, kind, _message(error), robot,
                                       source=source, scene=selected, phase=phase,
                                       destination=None if destination is None else str(destination),
                                       notes=list(_error_notes(error)),
                                       artifact_published=session is not None and session.result is not None)
    return _single_result(native, session.result, robot, source, selected)
