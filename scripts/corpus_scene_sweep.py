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
    "tex_unsupported",
    "missing_dependency",
    "missing_asset",
    "scene_code_error",
    "portal_exception",
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
    if returncode == 4 or "capability/" in tail:
        return "capability_refusal", tail[:300]
    if re.search(r"UnsupportedCommand|TexError|fmd-math|is not yet supported", stderr):
        return "tex_unsupported", tail[:300]
    if re.search(r"FileNotFoundError|No such file|asset", tail):
        return "missing_asset", tail[:300]
    if returncode < 0 or returncode > 100:
        return "crash", f"exit {returncode}"
    # A traceback whose last frame is inside the portal package is ours.
    frames = re.findall(r'File "([^"]+)", line \d+', stderr)
    if frames and ("manimlib" in frames[-1] or "fmn_python" in frames[-1]):
        return "portal_exception", tail[:300]
    return "scene_code_error", tail[:300]


def run_one(args, rel, scene, out_dir):
    target = out_dir / "frames" / f"{rel.replace('/', '__')}__{scene}.png"
    target.parent.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, PYTHONPATH=str(args.videos))
    start = time.monotonic()
    try:
        proc = subprocess.run(
            [args.portal, rel, scene, "--format", "png", "--resolution", "320x180",
             "--video_dir", str(target)],
            cwd=args.videos, env=env, capture_output=True, text=True,
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
    args = parser.parse_args()
    args.videos = args.videos.resolve()
    first, _, last = args.years.partition("-")
    years = range(int(first), int(last or first) + 1)

    scenes = enumerate_scenes(args.videos, years)
    if args.limit:
        scenes = scenes[: args.limit]
    args.out.mkdir(parents=True, exist_ok=True)
    print(f"enumerated {len(scenes)} scene classes in {args.years}", file=sys.stderr)

    records = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        futures = [pool.submit(run_one, args, rel, scene, args.out) for rel, scene in scenes]
        for done, future in enumerate(concurrent.futures.as_completed(futures), 1):
            records.append(future.result())
            if done % 50 == 0:
                print(f"{done}/{len(scenes)}", file=sys.stderr)

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
