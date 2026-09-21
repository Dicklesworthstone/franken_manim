"""Fresh-scene subdivision, sharing the live clip controller and native clock."""
from __future__ import annotations

import importlib
import os
from typing import Any

from .render_selection import animation_range as _animation_range
from .subdivision import SubdividedRecordingSession, SubdividedRenderResult


def subdivided_render_session(
    scene: Any, destination: os.PathLike[str] | str, *, format: str = "gif",
    resolution: tuple[int, int] | None = None, fps: int | None = None,
    threads: int | None = None, animation_range=None,
    max_segments: int = 10_000, _native: Any = None,
) -> SubdividedRecordingSession:
    """Own per-call clips on a pristine Scene with an explicitly configured FPS.

    This context never runs setup/construct/tear_down itself. For a populated
    scene, use record_subdivided_scene, which preserves the existing clock.
    """
    return SubdividedRecordingSession(
        scene, destination, format=format, resolution=resolution, fps=fps,
        threads=threads, max_segments=max_segments, _native=_native,
        _prepare=True, _selection=_animation_range(animation_range),
    )


def render_subdivided_scene(
    scene: Any, destination: os.PathLike[str] | str, *, format: str = "gif",
    resolution: tuple[int, int] | None = None, fps: int | None = None,
    threads: int | None = None, scene_kwargs: dict[str, Any] | None = None,
    animation_range=None, max_segments: int = 10_000,
) -> SubdividedRenderResult:
    """Run one Scene lifecycle, emitting a native clip for each selected call.

    Constructor, setup, construct and tear_down run only once. EndScene is
    successful range termination, not an output failure. Earlier clips remain
    published after execution failure; no scene or filesystem rollback occurs.
    """
    selection = _animation_range(animation_range)
    native = importlib.import_module("manimlib")
    if isinstance(scene, type) and issubclass(scene, native.Scene):
        scene = scene(**({} if scene_kwargs is None else dict(scene_kwargs)))
    elif scene_kwargs is not None:
        raise TypeError("scene_kwargs is valid only when rendering a Scene class")
    session = subdivided_render_session(
        scene, destination, format=format, resolution=resolution, fps=fps,
        threads=threads, animation_range=selection, max_segments=max_segments,
        _native=native,
    )
    with session:
        try:
            scene.run()
        except native.EndScene:
            pass
    if session.result is None:
        raise RuntimeError("scene execution ended without completing its subdivided generation")
    return session.result


def take_subdivision_option(options: list[str], value_flags) -> tuple[list[str], bool]:
    """Consume only --subdivide; never interpret another option's value."""
    forwarded, selected, index = [], False, 0
    while index < len(options):
        option = options[index]
        if option == "--subdivide":
            if selected:
                raise ValueError("--subdivide must not be repeated")
            selected = True
        else:
            forwarded.append(option)
            if option in value_flags and index + 1 < len(options):
                index += 1
                forwarded.append(options[index])
        index += 1
    return forwarded, selected


SUBDIVISION_HELP = """Per-call clip output:
  --subdivide             One native clip for each selected play() or wait().

Use one named scene with --format gif, y4m, or png_sequence. --video_dir is a
new collection directory, not an output filename. Source ranges retain original
segment indices; nested AnimationGroups remain one clip. --skip_animations,
write-all/multiple scenes, checkpoint recovery and certified output are not
combined with subdivision. Completed clips survive later execution failure.
The terminal robot receipt includes subdivision.segments and partial outputs.
"""
