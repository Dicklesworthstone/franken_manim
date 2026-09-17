"""Read-only Studio for existing Python scenes, using the native Studio host.

The selected source runs in a disposable instance of *this* host interpreter.
The supervisor does not import scene code. Captures are bounded, native PNGs
and native inspectors; scrubbing never executes source again. The Studio UI's
Reload button creates a fresh process, including fresh helper-module imports.
This is process isolation for robustness, not a Python security sandbox.
"""
from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import secrets
import sys
import time
from typing import Any

from . import _ensure_exclusive_manimlib_namespace

_SCHEMA = "fmn-python.studio-worker"
_MAX_SOURCE_BYTES = 16 * 1024 * 1024
_DEFAULT_FRAMES = 7200
_DEFAULT_BYTES = 256 * 1024 * 1024


class _Parser(argparse.ArgumentParser):
    def error(self, message):
        raise ValueError(message)


def _integer(value: Any, name: str, low: int, high: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not low <= value <= high:
        raise ValueError(f"{name} must be an integer in {low}..{high}")
    return value


def _source(path: Path) -> bytes:
    if path.suffix != ".py" or not path.is_file():
        raise ValueError("Studio source must be an existing .py file")
    with path.open("rb") as stream:
        data = stream.read(_MAX_SOURCE_BYTES + 1)
    if len(data) > _MAX_SOURCE_BYTES:
        raise ValueError("Studio source exceeds the 16 MiB source budget")
    # Compilation has no authored effects. Catch syntax/encoding errors before
    # replacing a working preview on an explicit Reload.
    compile(data, str(path), "exec", dont_inherit=True)
    return data


def _file_digest(path: Path) -> str:
    digest = hashlib.sha256()
    budget = 512 * 1024 * 1024
    with path.open("rb") as stream:
        before = os.fstat(stream.fileno())
        if before.st_size > budget:
            raise ValueError("Studio runtime component exceeds the 512 MiB identity budget")
        while chunk := stream.read(min(1024 * 1024, budget + 1)):
            budget -= len(chunk)
            if budget < 0:
                raise ValueError("Studio runtime component grew past the identity budget")
            digest.update(chunk)
        after = os.fstat(stream.fileno())
    if (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
        raise RuntimeError("Studio runtime changed while reading its identity")
    return digest.hexdigest()


def _canonical(value: dict[str, Any]) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True, allow_nan=False)


def _build_id(request: dict[str, Any]) -> str:
    return hashlib.sha256(_canonical({k: v for k, v in request.items() if k != "build_id"}).encode()).hexdigest()


class Studio:
    """An authenticated loopback preview. Close it, or use it as a context manager.

    Construction executes the selected scene once in a disposable worker.
    ``url`` contains a bearer capability: do not share it with untrusted users.
    Preview is silent/read-only; ordinary offline rendering remains separate.
    """
    def __init__(
        self, source: os.PathLike[str] | str, scene: str, *,
        resolution: tuple[int, int] = (640, 360), fps: int = 30, threads: int = 1,
        max_frames: int = _DEFAULT_FRAMES, max_bytes: int = _DEFAULT_BYTES,
        timeout: int = 120, port: int = 0,
    ) -> None:
        _ensure_exclusive_manimlib_namespace()
        from manimlib import _native

        if not isinstance(scene, str) or not scene.isidentifier() or len(scene) > 512:
            raise ValueError("Studio requires one explicit Scene class name")
        path = Path(source).resolve()
        _source(path)
        try:
            width, height = resolution
        except (TypeError, ValueError):
            raise ValueError("resolution must contain width and height") from None
        width = _integer(width, "width", 1, 16384)
        height = _integer(height, "height", 1, 16384)
        if width * height > 16_777_216:
            raise ValueError("Studio resolution exceeds the 16M-pixel capture budget")
        options = {
            "width": width, "height": height, "fps": _integer(fps, "fps", 1, 240),
            "threads": _integer(threads, "threads", 1, 96),
            "max_frames": _integer(max_frames, "max_frames", 1, 100000),
            "max_bytes": _integer(max_bytes, "max_bytes", 1, 1024 * 1024 * 1024),
        }
        timeout = _integer(timeout, "timeout", 1, 900)
        port = _integer(port, "port", 0, 65535)
        # abspath, NOT realpath: resolving a virtualenv interpreter symlink
        # would discard that environment's installed engine and dependencies.
        executable = Path(os.path.abspath(sys.executable))
        extension = Path(_native.__file__).resolve()
        runtime = {"python": _file_digest(executable), "native": _file_digest(extension),
                   "portal": _file_digest(Path(__file__).resolve()),
                   "abi": sys.implementation.cache_tag}
        # Freeze environment and interpreter authority at the host front door.
        # This is not a claim to record the complete C1-C10 certified closure.
        environment = sorted(os.environ.items())

        def rebuild():
            data = _source(path)
            request = {"schema": _SCHEMA, "version": 1, "source": str(path), "scene": scene,
                       "source_sha256": hashlib.sha256(data).hexdigest(),
                       "runtime": runtime, **options}
            request["build_id"] = _build_id(request)
            return (str(executable), ["-I", "-m", "fmn_python.studio", "--worker", _canonical(request)],
                    environment, str(path.parent), request["build_id"])

        self._host = _native._StudioHost(rebuild, scene, secrets.token_hex(32), port, timeout)

    @property
    def url(self) -> str:
        return self._host.url

    @property
    def alive(self) -> bool:
        return self._host.alive

    def close(self) -> None:
        self._host.close()

    def __enter__(self) -> Studio:
        if not self.alive:
            raise RuntimeError("Studio is closed")
        return self

    def __exit__(self, *_exc) -> None:
        self.close()


def _capture(request: dict[str, Any], native: Any):
    from .scene_loading import SceneSource
    from .rendering import _seed

    path = Path(request["source"])
    if hashlib.sha256(_source(path)).hexdigest() != request["source_sha256"]:
        raise RuntimeError("Studio source changed between launch and capture; reload")
    with SceneSource(path, native.Scene) as loaded:
        if loaded.source_digests.get(path) != request["source_sha256"]:
            raise RuntimeError("Studio executed source differs from the requested generation")
        if request["scene"] not in loaded.scenes:
            raise ValueError(f"Scene {request['scene']!r} was not defined in {path.name}")
        scene = loaded.scenes[request["scene"]]()
        # No source rewrite, no constructor kwargs, no duplicated frame clock.
        scene._begin_studio_capture(
            request["scene"], request["build_id"], request["source_sha256"],
            request["width"], request["height"], request["fps"], request["threads"],
            _seed(scene.random_seed), request["max_frames"], request["max_bytes"],
        )
        try:
            scene.camera._core.set_pixel_shape(request["width"], request["height"])
            scene.camera.fps = request["fps"]
            try:
                scene.run()
            except native.EndScene:
                pass
            return scene._finish_studio_capture()
        except BaseException:
            scene._abort_render()
            raise


def _worker(encoded: str) -> int:
    if len(encoded) > 65536:
        raise ValueError("Studio worker request exceeds its launch budget")
    request = json.loads(encoded)
    if not isinstance(request, dict) or request.get("schema") != _SCHEMA or request.get("version") != 1:
        raise ValueError("unsupported Studio worker request")
    if request.get("build_id") != _build_id(request):
        raise ValueError("Studio worker request identity mismatch")
    # Authored stdout must never enter the framed native protocol. Low-level
    # writes to fd 1 remain arbitrary Python authority, not a sandbox promise;
    # corrupted framing is rejected by the supervisor.
    with contextlib.redirect_stdout(sys.stderr):
        from manimlib import _native
        actual = {"python": _file_digest(Path(sys.executable)),
                  "native": _file_digest(Path(_native.__file__).resolve()),
                  "portal": _file_digest(Path(__file__).resolve()),
                  "abi": sys.implementation.cache_tag}
        if request["runtime"] != actual:
            raise RuntimeError("Studio worker interpreter/engine differs from the selected host runtime")
        recording = _capture(request, _native)
    recording.serve()
    return 0


_HELP = """Read-only native Studio for Python scenes:
  fmn-python studio SOURCE.py SCENE [--resolution WxH] [--fps N] [--threads N]
                    [--max_frames N] [--max_bytes N] [--timeout SECONDS] [--port N]

Executes once in a disposable host-CPython worker; scrub and inspect the captured
frames in the authenticated native Studio UI. Reload explicitly executes fresh
source and imports. Ctrl-C closes the host and worker. This is not a sandbox.
No live Python event editing, sound playback, callback checkpoints, or certified
render claim. Default: 640x360, 30 FPS, 7200 frames, 256 MiB encoded capture budget.
"""


def try_studio_cli(native: Any, arguments: list[str]) -> int | None:
    args = list(arguments)
    robot = "--robot" in args
    args = [a for a in args if a != "--robot"]
    if not args or args[0] != "studio":
        return None
    if "--help" in args or "-h" in args:
        if robot:
            return native._portal_cli_emit(0, "success", "help", "Python Studio usage", True, help=_HELP)
        print(_HELP)
        return 0
    # These are preview resource controls, not a replacement scene/config parser.
    # Rendering itself still resolves the existing native camera/runtime config.
    parser = _Parser(prog="fmn-python studio", add_help=False, exit_on_error=False, allow_abbrev=False)
    parser.add_argument("source")
    parser.add_argument("scene")
    parser.add_argument("--resolution", default="640x360")
    for flag, default in (("fps", 30), ("threads", 1), ("max_frames", _DEFAULT_FRAMES),
                          ("max_bytes", _DEFAULT_BYTES), ("timeout", 120), ("port", 0)):
        parser.add_argument("--" + flag, type=int, default=default)
    phase = "arguments"
    try:
        options, extra = parser.parse_known_args(args[1:])
        if extra:
            raise ValueError("unsupported Studio options: " + " ".join(extra))
        dimensions = tuple(int(v) for v in options.resolution.lower().split("x"))
        config = vars(options).copy()
        config["resolution"] = dimensions
        phase = "start"
        with Studio(**config) as host:
            if robot:
                native._portal_cli_emit(0, "success", "studio", "Python Studio is ready", True,
                                       url=host.url, scene=options.scene, preview_mode="captured-read-only",
                                       certified=False)
            else:
                print("Python Studio (read-only): " + host.url)
                print("Reload runs fresh source. Ctrl-C closes Studio.", file=sys.stderr)
            sys.stdout.flush()
            phase = "serve"
            while host.alive:
                time.sleep(0.05)
        return 0
    except KeyboardInterrupt:
        return 130
    except (ValueError, OSError, RuntimeError, SyntaxError, argparse.ArgumentError, SystemExit) as error:
        code = 2 if phase == "arguments" else 6
        return native._portal_cli_emit(code, "usage" if code == 2 else "execution", "studio", str(error)[:4096], robot, phase=phase)


if __name__ == "__main__":
    try:
        if len(sys.argv) != 3 or sys.argv[1] != "--worker":
            raise ValueError("use fmn-python studio SOURCE.py SCENE")
        raise SystemExit(_worker(sys.argv[2]))
    except Exception as error:
        print("Python Studio worker failed: " + str(error)[:4096], file=sys.stderr)
        raise SystemExit(6) from None
