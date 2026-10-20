"""Single-scene clip recovery over the existing parser, loader and render owner."""
from __future__ import annotations

from contextlib import nullcontext, redirect_stdout
import hashlib
import os
from pathlib import Path
import sys

from .batch_checkpoint import _open_regular
from .batch_rendering import _error_fields, _error_notes
from .checkpoint_cli import take_checkpoint_options
from .console_rendering import _VALUE_FLAGS, _output_overrides, _output_paths, _single_destination
from .render_selection import take_playback_options
from .rendering import _positive_integer
from .scene_loading import SceneSource, resolve_render_configuration
from .subdivision import SubdividedRenderResult
from .subdivision_rendering import (
    render_subdivided_scene, take_subdivision_option, validate_subdivided_mode,
)
from .video_cli import take_video_options

_FLAG = "--clip-checkpoint"
_VALUES = _VALUE_FLAGS | {_FLAG}
_FORBIDDEN = frozenset({
    "--checkpoint", "--workers", "--job-timeout", "--write_all", "-a", "--keep-going",
    "--save-last-frame", "--reproducible", "--list-scenes", "--construct-only",
    "--audit-parity", "--version",
})
_HELP = """Resumable clips within one scene:
  fmn-python SOURCE.py SCENE --subdivide --clip-checkpoint JOURNAL
             --resume-key INPUTS [--resume] [--format png_sequence|gif|y4m|wav|mp4|mov]

The journal and its persistent lock must be outside the clip directory.
Resume checks every saved clip, then reexecutes scene code and animation updates
without rendering the saved prefix. Python side effects repeat. The scene's
call indices, kinds and timing must match before new clips can publish. INPUTS
is your version for code, helpers, assets and runtime, not a certified closure.
Entry-source bytes, selected class, cwd and output options also bind the plan.
An unrecorded existing clip is never adopted, removed or overwritten.
This is single-scene recovery, distinct from the batch --checkpoint option.
Do not combine it with --workers, --write_all, --save-last-frame or --reproducible.
"""


def _groups(arguments):
    """Retain option/value ownership even when a value resembles a switch."""
    index = 0
    while index < len(arguments):
        token = arguments[index]
        size = 2 if token in _VALUES and index + 1 < len(arguments) else 1
        yield arguments[index:index + size]
        index += size


def _digest(path):
    # No FIFO/device blocking before source admission. Check the exact entry
    # compilation bytes in SceneSource too, including package initializer edits.
    with _open_regular(path) as stream:
        raw = stream.read(16 * 1024 * 1024 + 1)
    if len(raw) > 16 * 1024 * 1024:
        raise ValueError("clip scene entry source exceeds its 16 MiB budget")
    return hashlib.sha256(raw).hexdigest()


def try_clip_recovery_cli(native, arguments):
    """Consume only an explicit clip journal; ordinary invocations delegate."""
    groups = list(_groups(arguments))
    heads = [group[0].partition("=")[0] for group in groups]
    if _FLAG not in heads:
        return None
    robot = "--robot" in heads
    if "--help" in heads or "-h" in heads:
        if robot:
            return native._portal_cli_emit(0, "success", "help", "clip checkpoint usage", True, help=_HELP)
        print(_HELP)
        return 0
    try:
        if any(group[0].startswith("--robot=") for group in groups):
            raise ValueError("--robot does not take a value")
        if heads.count(_FLAG) != 1 or heads.count("--robot") > 1:
            raise ValueError("clip checkpoint and robot switches must not be repeated")
        forbidden = _FORBIDDEN.intersection(heads)
        if forbidden:
            raise ValueError("--clip-checkpoint cannot combine with " + ", ".join(sorted(forbidden)))
        invocation_directory = Path.cwd()
        forwarded = []
        for group in groups:
            flag, separator, value = group[0].partition("=")
            if flag == _FLAG:
                # Reuse the shared checkpoint lexer, including required keys,
                # duplicate detection and freezing its path before source runs.
                forwarded.extend(["--checkpoint", value] if separator else ["--checkpoint", *group[1:]])
            elif flag != "--robot":
                forwarded.extend(group)
        forwarded, recovery = take_checkpoint_options(forwarded, _VALUES)
        forwarded, video = take_video_options(forwarded, _VALUES)
        forwarded, subdivide = take_subdivision_option(forwarded, _VALUES)
        if not subdivide:
            raise ValueError("--clip-checkpoint requires --subdivide")
        # The shared playback parser sees only actual playback tokens, never
        # an encoder/path/key value with the spelling '-n' or '-s'.
        playback, remaining = [], []
        for group in _groups(forwarded):
            token = group[0]
            is_playback = (token in {"-s", "--skip_animations", "-n", "--start_at_animation_number"}
                           or token.startswith("--start_at_animation_number=")
                           or (token.startswith("-n") and len(token) > 2
                               and token[2].isascii() and token[2].isdigit()))
            (playback if is_playback else remaining).extend(group)
        _, selection, still = take_playback_options(playback)
        if still:
            raise ValueError("clip recovery cannot combine with --skip_animations/-s")
        positionals, options, width, height, fps, threads = native._portal_cli_render_arguments(remaining)
        if len(positionals) != 2:
            raise ValueError("clip recovery requires SOURCE.py and exactly one explicit SCENE")
        source, selected = positionals
        if not selected.isidentifier() or len(selected.encode("utf-8")) > 128:
            raise ValueError("clip recovery scene must be an identifier of at most 128 UTF-8 bytes")
        options.update(video)
        configuration, width, height, fps = resolve_render_configuration(native, options, width, height, fps)
        output_options = _output_overrides(options)
        validate_subdivided_mode(native, options["format"], reproducible=bool(options.get("reproducible")))
        for name, value in (("width", width), ("height", height), ("fps", fps), ("threads", threads)):
            if value is not None:
                _positive_integer(value, name)
        if width * height > 16_777_216 or (threads is not None and threads > 96):
            raise ValueError("clip output requires at most 16777216 pixels and 1..96 threads")
        source_path = Path(source).resolve()
        if source_path.suffix.lower() not in {".py", ".pyw"}:
            raise ValueError("clip recovery requires a .py or .pyw source")
        root, exact = _output_paths(options, source_path)
        supplied = _single_destination(root, exact, selected, options, subdivide=True)
        text = os.fspath(supplied)
        if not isinstance(text, str) or not text or "\0" in text:
            raise ValueError("clip output must be a nonempty text path without NUL")
        # Never resolve a leaf link into permission to reuse its target.
        destination = Path(text).absolute()
        destination = destination.parent.resolve() / destination.name
        from .clip_checkpoint import ClipCheckpoint
        journal = ClipCheckpoint(recovery["checkpoint"], resume=recovery["resume"], key=recovery["resume_key"])
        journal.validate_destinations([destination])
        digest = _digest(source_path)
        source_identity = {"path": str(source_path), "sha256": digest,
                           "scene": selected, "cwd": str(invocation_directory)}
        if configuration is not None:
            source_identity["configuration"] = configuration
    except (OSError, TypeError, ValueError, RuntimeError) as error:
        message = _error_fields(error)[1]
        code = 4 if message.startswith("CAPABILITY: ") else 2
        return native._portal_cli_emit(code, "capability" if code == 4 else "usage",
            "render-capability-unavailable" if code == 4 else "usage-error", message, robot)

    phase = "load"
    try:
        with (redirect_stdout(sys.stderr) if robot else nullcontext(),
              SceneSource(source_path, native.Scene, entry_digest=digest, configuration=configuration) as loaded):
            if selected not in loaded.scenes:
                raise ValueError(f"scene {selected!r} was not declared by {source}")
            phase = "render"
            result = render_subdivided_scene(
                loaded.scenes[selected], destination, format=options["format"],
                resolution=(width, height), fps=fps, threads=threads,
                animation_range=selection, _output_options=output_options,
                _source_identity=source_identity, **recovery,
            )
    except BaseException as error:
        if isinstance(error, (KeyboardInterrupt, SystemExit)):
            code, identity, kind = (130 if isinstance(error, KeyboardInterrupt) else 5), "interrupted", "render-interrupted"
        elif not isinstance(error, Exception):
            raise
        elif _error_fields(error)[1].startswith("CAPABILITY: "):
            code, identity, kind = 4, "capability", "render-capability-unavailable"
        elif phase == "render" and isinstance(error, OSError):
            code, identity, kind = 6, "render", "clip-checkpoint-failed"
        else:
            code, identity, kind = 5, "scene", "scene-load-failed" if phase == "load" else ("clip-replay-failed" if recovery["resume"] else "clip-render-failed")
        partial = getattr(error, "render_subdivision_result", None)
        detail = {"subdivision": partial.as_dict()} if isinstance(partial, SubdividedRenderResult) else {}
        if isinstance(partial, SubdividedRenderResult) and (partial.segments or partial.unreceipted_artifacts):
            detail["artifact_published"] = True
        # An empty receipt after preflight failure is not proof that no files
        # exist: there may be an unrecorded output or an unverifiable old clip.
        return native._portal_cli_emit(code, identity, kind, _error_fields(error)[1], robot,
            source=source, scene=selected, destination=str(destination), checkpoint=str(journal.path),
            resume_requested=recovery["resume"], notes=list(_error_notes(error)), **detail)
    if options.get("quiet") and not robot:
        return 0
    return native._portal_cli_emit(0, "success", "render-subdivided",
        f"completed {len(result.segments)} checkpointed clips in {destination}", robot,
        source=source, scene=selected, destination=str(destination), rendered=True,
        checkpoint=str(journal.path), resume_requested=recovery["resume"], subdivision=result.as_dict())
