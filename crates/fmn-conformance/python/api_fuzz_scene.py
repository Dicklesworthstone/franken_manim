"""The differential API fuzzer's batch scene (fm-5wq.34).

This module runs **unmodified** under the pinned Reference and under the
portal, through each engine's own CLI in skip mode (`-s`). Its one scene runs
every program of a batch in turn: each program executes in a fresh namespace
holding the engine's `manimlib` exports and the running scene as `self`, then
its mobject variables are measured with the shared structural-facts extractor
(fm-5wq.36) and the scene is cleared for the next program.

Environment:
  FMN_FUZZ_PROGRAMS  NDJSON file of programs ({"id", "source", "roots"})
  FMN_FUZZ_FACTS     output NDJSON, one facts record per program
  FMN_FUZZ_TOOLS     directory holding structural_facts.py
  FMN_FUZZ_TIMEOUT   seconds per program (default 10)
"""
import hashlib
import json
import os
import signal
import sys

sys.path.insert(0, os.environ["FMN_FUZZ_TOOLS"])

import manimlib  # noqa: E402
import numpy  # noqa: E402
import structural_facts as sf  # noqa: E402

EXPORTS = {name: getattr(manimlib, name) for name in dir(manimlib) if not name.startswith("_")}
TIMEOUT = int(os.environ.get("FMN_FUZZ_TIMEOUT", "10"))


class ProgramTimeout(Exception):
    pass


def _alarm(signum, frame):
    raise ProgramTimeout(f"program exceeded {TIMEOUT} s")


def run_program(scene, program, baseline):
    """Facts for one program: its named mobjects in creation order, then the scene list.

    `baseline` is what the scene held before any program ran (the Reference lists
    its camera frame there). It is left out of the scene fact and left in place
    afterwards, so a program's facts never depend on its position in the batch."""
    namespace = dict(EXPORTS)
    namespace["np"] = numpy
    namespace["self"] = scene
    previous = signal.signal(signal.SIGALRM, _alarm)
    signal.alarm(TIMEOUT)
    try:
        # Executing the program is the point. FMN_FUZZ_PROGRAMS is the generator's own
        # output; never point it at a file from anywhere else.
        exec(compile(program["source"], program["id"], "exec"), namespace)  # noqa: S102
        signal.alarm(0)
        roots = [namespace[name] for name in program["roots"] if name in namespace]
        record = sf.extract(program["id"], roots, namespace=manimlib, points_mode="full")
        _note_stale_boxes(record, roots)
        record["absent_roots"] = [name for name in program["roots"] if name not in namespace]
        added = [mob for mob in scene.mobjects if not any(mob is kept for kept in baseline)]
        record["scene"] = [program["roots"].index(name) if name in program["roots"] else -1
                           for name in _names_of(added, namespace, program["roots"])]
    except BaseException as error:  # noqa: BLE001 - every outcome is a fact
        signal.alarm(0)
        if isinstance(error, (KeyboardInterrupt, SystemExit)):
            raise
        record = sf.extract_error(program["id"], error)
    finally:
        signal.signal(signal.SIGALRM, previous)
    try:
        scene.remove(*[mob for mob in scene.mobjects if not any(mob is kept for kept in baseline)])
        for mob in baseline:
            if not any(mob is present for present in scene.mobjects):
                scene.add(mob)
    except Exception:  # noqa: BLE001
        pass
    return record


def _members(roots):
    """(path, mobject) in sf.extract's depth-first order."""
    stack = [(str(index), root) for index, root in reversed(list(enumerate(roots)))]
    while stack:
        path, mob = stack.pop()
        yield path, mob
        children = list(getattr(mob, "submobjects", ()) or ())
        stack.extend((f"{path}.{i}", child) for i, child in reversed(list(enumerate(children))))


def _note_stale_boxes(record, roots):
    """Record, on each member whose reported box is not its family's point box, the
    point box itself (`bbox_points`). Appendix C-35: the Reference's box ignores an
    in-place write through a live point view; the fuzzer's diff uses this to tell
    that stale box from a moved object."""
    members = {m["path"]: m for m in record.get("members", ())}
    for path, mob in _members(roots):
        member = members.get(path)
        if member is None or not isinstance(member.get("bbox"), list):
            continue
        try:
            blocks = [m.get_points() for m in mob.get_family()]
            blocks = [b for b in blocks if len(b)]
            if not blocks:
                continue
            every = numpy.vstack(blocks)
            low, high = every.min(axis=0), every.max(axis=0)
            fresh = sf.quantize([low, (low + high) / 2, high])
        except Exception:  # noqa: BLE001 - a family that cannot be read has no point box
            continue
        if not sf._within_tolerance(fresh, member["bbox"]):
            member["bbox_points"] = fresh


def _names_of(mobjects, namespace, roots):
    """The root variable naming each scene-list member, or None for an unnamed one."""
    by_id = {id(namespace[name]): name for name in roots if name in namespace}
    return [by_id.get(id(mob)) for mob in mobjects]


def engine_header():
    """Which engine measured the batch: the portal names itself and its native
    extension's digest; the Reference is identified by its source commit, which
    the driver adds."""
    native = getattr(getattr(manimlib, "manimlib", None), "__file__", None)
    digest = None
    if native and getattr(manimlib, "__franken_manim__", False):
        with open(native, "rb") as image:
            digest = hashlib.sha256(image.read()).hexdigest()[:16]
    return {"engine": str(getattr(manimlib, "__engine__", "reference")),
            "version": str(getattr(manimlib, "__version__", "unknown")),
            "native_sha256": digest, "python": sys.version.split()[0], "numpy": numpy.__version__}


class FuzzBatch(manimlib.Scene):
    def construct(self):
        with open(os.environ["FMN_FUZZ_PROGRAMS"], encoding="utf-8") as source, \
                open(os.environ["FMN_FUZZ_FACTS"], "w", encoding="utf-8") as sink:
            sink.write("# " + sf.canonical(engine_header()) + "\n")
            baseline = list(self.mobjects)
            for line in source:
                if not line.strip():
                    continue
                record = run_program(self, json.loads(line), baseline)
                sink.write(sf.canonical(record) + "\n")
                sink.flush()
