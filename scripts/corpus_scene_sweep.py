#!/usr/bin/env python3
"""Sweep real 3b1b scenes through an installed fmn-python portal (fm-5wq.14).

The README promises that existing manim scenes run source-unedited. Until
this sweep, the only measurement was eight allowlisted seed scenes. This
tool enumerates every Scene subclass in the pinned 3b1b/videos checkout for
the chosen years, renders each one's final frame through the portal with a
timeout, and classifies the outcome with a closed vocabulary. A portal-side
Python exception is a bug, never a refusal.

The corpus is licensing-restricted (CC BY-NC-SA): records carry scene
identifiers and outcome codes only, never scene source.

Usage:
    corpus_scene_sweep.py --portal PATH/TO/fmn-python --videos scripts/videos_ref \
        --years 2020-2026 --out DIR [--jobs 16] [--timeout 180] [--limit N]

Writes DIR/records.ndjson (one JSON record per scene, sorted) and prints a
summary; `--summary-tsv` also writes the per-outcome counts.

The portal CLI reports one structured error line without a traceback, so an
ordinary exception is first recorded as `runtime_error` (origin unknown).
`--attribute --python PATH/TO/portal/python` re-runs each of those scenes
in-process and records which code raised it (`raised_in`: portal or scene),
splitting them into `portal_exception` and `scene_code_error`. Raised-in is
where the exception surfaced, not proof of fault: an AttributeError raised in
scene code can still come from the portal returning the wrong type.
"""
import argparse
import ast
import concurrent.futures
import json
import os
import pathlib
import re
import subprocess
import sys
import time

SCENE_ROOTS = {"Scene", "ThreeDScene", "InteractiveScene"}

# Closed outcome vocabulary, in precedence order.
OUTCOMES = (
    "ok",
    "capability_refusal",
    "portal_unbound",
    "tex_unsupported",
    "missing_dependency",
    "missing_asset",
    "scene_code_error",
    "portal_exception",
    "runtime_error",
    "timeout",
    "crash",
)


def enumerate_scenes(videos: pathlib.Path, years: range):
    """Scene subclasses by static AST analysis.

    A class is a scene when a base is a scene root or a scene class known in
    the same module; imported custom bases ending in "Scene" also count.
    Classes whose names start with "_" and abstract helpers without a
    construct() anywhere in their module lineage are still attempted: the
    portal's own verdict is the measurement.
    """
    scenes = []
    for year in years:
        root = videos / f"_{year}"
        if not root.is_dir():
            continue
        for path in sorted(root.rglob("*.py")):
            try:
                tree = ast.parse(path.read_text(encoding="utf-8"))
            except (SyntaxError, UnicodeDecodeError):
                continue
            bases = {}
            for node in tree.body:
                if isinstance(node, ast.ClassDef):
                    names = []
                    for base in node.bases:
                        if isinstance(base, ast.Name):
                            names.append(base.id)
                        elif isinstance(base, ast.Attribute):
                            names.append(base.attr)
                    bases[node.name] = names
            known = set()
            changed = True
            while changed:
                changed = False
                for name, parents in bases.items():
                    if name in known:
                        continue
                    if any(p in SCENE_ROOTS or p in known or p.endswith("Scene") for p in parents):
                        known.add(name)
                        changed = True
            rel = path.relative_to(videos).as_posix()
            scenes.extend((rel, name) for name in sorted(known))
    return scenes


def classify(returncode, stderr, timed_out):
    if timed_out:
        return "timeout", "exceeded timeout"
    if returncode == 0:
        return "ok", ""
    tail = stderr.strip().splitlines()[-1] if stderr.strip() else ""
    missing = re.search(r"No module named '([^']+)'", stderr)
    if missing and "manimlib" not in missing.group(1) and "fmn_python" not in missing.group(1):
        return "missing_dependency", missing.group(1)
    # fmn-python: scene/<kind>: <ExceptionType>: <message>
    parsed = re.search(r"scene/[a-z-]+: ([A-Za-z_.]+): (.*)", tail)
    exc_type = parsed.group(1).rsplit(".", 1)[-1] if parsed else ""
    if "semantic binding has not landed" in tail:
        return "portal_unbound", tail[:300]
    if (
        returncode == 4
        or "capability/" in tail
        or exc_type in ("NotImplementedError", "CapabilityError")
    ):
        return "capability_refusal", tail[:300]
    if re.search(r"UnsupportedCommand|TexError|fmd-math|is not yet supported", stderr):
        return "tex_unsupported", tail[:300]
    # "<name> not Found" is ImageMobject's Reference-worded missing-file error.
    if re.search(r"FileNotFoundError|No such file|asset| not Found$", tail):
        return "missing_asset", tail[:300]
    if returncode < 0 or returncode > 100:
        return "crash", f"exit {returncode}"
    return "runtime_error", tail[:300]


# Re-run one scene in-process and report where its exception surfaced.
ATTRIBUTION_RUNNER = r"""
import importlib, json, os, sys, traceback
videos, rel, scene, dest = sys.argv[1:5]
sys.path.insert(0, videos)
module = rel[:-3].replace("/", ".")
try:
    cls = getattr(importlib.import_module(module), scene)
    cls().render(dest, format="png", resolution=(320, 180))
    print(json.dumps({"raised_in": "none"}))
except BaseException as error:
    origin, frames = "other", []
    for frame in reversed(traceback.extract_tb(error.__traceback__)):
        name = frame.filename
        if name.startswith(videos):
            where = "scene"
        elif "manimlib" in name or "fmn_python" in name:
            where = "portal"
        else:
            continue
        frames.append([where, os.path.basename(name), frame.lineno, frame.name])
        if origin == "other":
            origin = where
    print(json.dumps({"raised_in": origin, "type": type(error).__name__, "frames": frames[:4]}))
"""


def scene_env(args):
    # The corpus root goes first; an inherited PYTHONPATH (e.g. a portal
    # build under test) stays importable after it.
    inherited = os.environ.get("PYTHONPATH")
    path = str(args.videos) + (os.pathsep + inherited if inherited else "")
    return dict(os.environ, PYTHONPATH=path)


def attribute(args, record, out_dir):
    target = out_dir / "attribution" / f"{record['module'].replace('/', '__')}__{record['scene']}.png"
    target.parent.mkdir(parents=True, exist_ok=True)
    try:
        proc = subprocess.run(
            [args.python, "-c", ATTRIBUTION_RUNNER, str(args.videos), record["module"],
             record["scene"], str(target)],
            cwd=args.workdir, env=scene_env(args), capture_output=True, text=True,
            timeout=args.timeout,
        )
        found = json.loads(proc.stdout.strip().splitlines()[-1])
    except (subprocess.TimeoutExpired, ValueError, IndexError):
        return record
    record = dict(record, raised_in=found["raised_in"], frames=found.get("frames", []))
    if found["raised_in"] == "portal":
        record["outcome"] = "portal_exception"
    elif found["raised_in"] == "scene":
        record["outcome"] = "scene_code_error"
    return record


def run_one(args, rel, scene, out_dir):
    target = out_dir / "frames" / f"{rel.replace('/', '__')}__{scene}.png"
    target.parent.mkdir(parents=True, exist_ok=True)
    env = scene_env(args)
    start = time.monotonic()
    try:
        proc = subprocess.run(
            [args.portal, str(args.videos / rel), scene, "--format", "png",
             "--resolution", "320x180", "--video_dir", str(target)],
            cwd=args.workdir, env=env, capture_output=True, text=True,
            timeout=args.timeout,
        )
        outcome, detail = classify(proc.returncode, proc.stderr, False)
        code = proc.returncode
    except subprocess.TimeoutExpired:
        outcome, detail, code = "timeout", f"exceeded {args.timeout}s", None
    return {
        "module": rel,
        "scene": scene,
        "outcome": outcome,
        "detail": detail,
        "exit": code,
        "seconds": round(time.monotonic() - start, 2),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--portal", required=True)
    parser.add_argument("--videos", required=True, type=pathlib.Path)
    parser.add_argument("--years", default="2020-2026")
    parser.add_argument("--out", required=True, type=pathlib.Path)
    parser.add_argument("--jobs", type=int, default=16)
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--summary-tsv", type=pathlib.Path)
    parser.add_argument("--attribute", action="store_true",
                        help="re-run runtime_error scenes in-process to find where they raised")
    parser.add_argument("--python", help="the portal environment's python (for --attribute)")
    parser.add_argument(
        "--config", type=pathlib.Path,
        help="a custom_config.yml to run under, as a user with their own directories "
             "would; default: the corpus's own (its base is the author's machine)",
    )
    args = parser.parse_args()
    if args.attribute and not args.python:
        parser.error("--attribute requires --python")
    args.videos = args.videos.resolve()
    first, _, last = args.years.partition("-")
    years = range(int(first), int(last or first) + 1)

    scenes = enumerate_scenes(args.videos, years)
    if args.limit:
        scenes = scenes[: args.limit]
    args.out.mkdir(parents=True, exist_ok=True)
    # Scenes run from a working directory whose custom_config.yml the portal
    # reads, exactly as the Reference reads the cwd's.
    args.workdir = args.videos
    if args.config:
        args.workdir = (args.out / "workdir").resolve()
        args.workdir.mkdir(exist_ok=True)
        (args.workdir / "custom_config.yml").write_text(
            args.config.read_text(encoding="utf-8"), encoding="utf-8"
        )
    print(f"enumerated {len(scenes)} scene classes in {args.years}", file=sys.stderr)

    records = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        futures = [pool.submit(run_one, args, rel, scene, args.out) for rel, scene in scenes]
        for done, future in enumerate(concurrent.futures.as_completed(futures), 1):
            records.append(future.result())
            if done % 50 == 0:
                print(f"{done}/{len(scenes)}", file=sys.stderr)

    if args.attribute:
        pending = [r for r in records if r["outcome"] == "runtime_error"]
        print(f"attributing {len(pending)} runtime errors", file=sys.stderr)
        keep = [r for r in records if r["outcome"] != "runtime_error"]
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
            keep.extend(pool.map(lambda r: attribute(args, r, args.out), pending))
        records = keep

    records.sort(key=lambda r: (r["module"], r["scene"]))
    with open(args.out / "records.ndjson", "w", encoding="utf-8") as handle:
        for record in records:
            handle.write(json.dumps(record, sort_keys=True) + "\n")

    counts = {name: 0 for name in OUTCOMES}
    for record in records:
        counts[record["outcome"]] += 1
    total = len(records)
    for name in OUTCOMES:
        share = 100.0 * counts[name] / total if total else 0.0
        print(f"{name}\t{counts[name]}\t{share:.1f}%")
    if args.summary_tsv:
        with open(args.summary_tsv, "w", encoding="utf-8") as handle:
            handle.write("outcome\tscenes\n")
            for name in OUTCOMES:
                handle.write(f"{name}\t{counts[name]}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
