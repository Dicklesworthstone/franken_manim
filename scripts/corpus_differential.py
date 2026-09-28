#!/usr/bin/env python3
"""Corpus differential (fm-5wq.33): the same corpus scenes in the pinned Reference and the portal.

The corpus sweep (corpus_scene_sweep.py) counts scenes that *run*. This tool asks whether
they *mean the same thing*. Each candidate scene runs through both engines' own CLIs under
`structural_facts.py run-scene`, which records the scene's structural facts at `tear_down`
(fm-5wq.36: one extractor, no scene edits). The facts are diffed in two tiers, both under
the BN-keyed exclusion table:
  structure   classes, public MRO, family shape, point counts, strings, style, scene facts
  geometry    structure plus points, bounding boxes and coordinate getters
BN-05 text metrics move everything laid out relative to text, so geometry equality is
reported separately from structural equality.

Both engines run in skip mode (-s: the final state, updaters per BN-10). Both final frames are kept, with SSIM and mean-absolute-difference smoke metrics. Those are
never gates. The corpus is CC BY-NC-SA, so records and the dashboard carry scene identifiers
and outcome codes only. The side-by-side gallery is written under --out, outside the repo.

Usage:
  corpus_differential.py --portal-python P --reference-python R --videos scripts/videos_ref \\
      --config custom_config.yml --out DIR [--candidates SWEEP.ndjson] [--jobs 16] \\
      [--timeout 240] [--years 2015-2026] [--limit N]
  corpus_differential.py --report DIR --dashboard docs/ratchet/differential_dashboard.md
"""
from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import html
import itertools
import json
import os
import pathlib
import sys
import threading
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
sys.path.insert(0, str(ROOT / "crates" / "fmn-conformance" / "python"))
from corpus_scene_sweep import enumerate_scenes, run_child  # noqa: E402
import structural_facts as sf  # noqa: E402

FACTS_TOOL = ROOT / "crates" / "fmn-conformance" / "python" / "structural_facts.py"
EXCLUSIONS = ROOT / "crates" / "fmn-conformance" / "fixtures" / "structural_facts" / "exclusions.json"
SCHEMA = "fmn.corpus-differential"
VERSION = 1
_displays = itertools.count(4100)
_display_lock = threading.Lock()


def _display():
    with _display_lock:
        return next(_displays)


def _slug(module: str, scene: str) -> str:
    return f"{module.replace('/', '__')}__{scene}"


def run_portal(args, module, scene, work):
    png = work / "portal.png"
    argv = [args.portal_python, str(FACTS_TOOL), "run-scene", "--facts", str(work / "portal.ndjson"),
            "--engine", "portal", "--", str(args.videos / module), scene, "-s",
            "--format", "png", "--resolution", "320x180", "--video_dir", str(png)]
    env = dict(os.environ, PYTHONPATH=str(args.videos))
    return run_child(argv, args.workdir, env, args.timeout), png


def run_reference(args, module, scene, work):
    media = work / "reference_media"
    env = {"PATH": "/usr/bin:/bin", "HOME": os.environ.get("HOME", "/tmp"),
           "PYTHONPATH": f"{args.reference_root}{os.pathsep}{args.videos}"}
    for attempt in range(3):
        argv = ["xvfb-run", "-n", str(_display()), "-s", "-screen 0 1920x1080x24",
                args.reference_python, str(FACTS_TOOL), "run-scene",
                "--facts", str(work / "reference.ndjson"), "--engine", "reference", "--",
                str(args.videos / module), scene, "-s", "-w", "-r", "320x180", "--video_dir", str(media)]
        result = run_child(argv, args.workdir, env, args.timeout)
        # xvfb-run exits 1 before starting Python when its display is taken; retry on a new one.
        if not (result[0] == 1 and "Xvfb failed" in result[1]):
            break
    pngs = sorted(media.rglob("*.png")) if media.exists() else []
    return result, (pngs[-1] if pngs else media / "absent.png")


def _facts(path):
    if not path.exists():
        return None
    records = sf.read_ndjson(path)
    return records[-1] if records else None


def _smoke(ref_png, portal_png):
    """SSIM (8x8 block statistics, luma) and mean absolute difference. Smoke alarms only."""
    if not (ref_png.exists() and portal_png.exists()):
        return None
    try:
        import numpy as np
        from PIL import Image
    except ImportError:
        return None
    a = np.asarray(Image.open(ref_png).convert("L").resize((320, 180)), dtype=np.float64)
    b = np.asarray(Image.open(portal_png).convert("L").resize((320, 180)), dtype=np.float64)
    blocks = lambda x: x[: x.shape[0] // 8 * 8, : x.shape[1] // 8 * 8].reshape(  # noqa: E731
        x.shape[0] // 8, 8, x.shape[1] // 8, 8).swapaxes(1, 2).reshape(-1, 64)
    xa, xb = blocks(a), blocks(b)
    ma, mb = xa.mean(1), xb.mean(1)
    va, vb = xa.var(1), xb.var(1)
    cov = ((xa - ma[:, None]) * (xb - mb[:, None])).mean(1)
    c1, c2 = (0.01 * 255) ** 2, (0.03 * 255) ** 2
    ssim = ((2 * ma * mb + c1) * (2 * cov + c2)) / ((ma**2 + mb**2 + c1) * (va + vb + c2))
    return {"ssim": round(float(ssim.mean()), 4), "mae": round(float(np.abs(a - b).mean()), 3)}


def _digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()[:16] if path.exists() else None


def run_one(args, module, scene):
    work = args.out / "scenes" / _slug(module, scene)
    work.mkdir(parents=True, exist_ok=True)
    start = time.monotonic()
    (p_code, p_err, _), portal_png = run_portal(args, module, scene, work)
    p_seconds = round(time.monotonic() - start, 1)
    start = time.monotonic()
    (r_code, r_err, _), ref_png = run_reference(args, module, scene, work)
    r_seconds = round(time.monotonic() - start, 1)
    portal, ref = _facts(work / "portal.ndjson"), _facts(work / "reference.ndjson")
    ran = {"portal": portal is not None and "error" not in portal and p_code == 0,
           "reference": ref is not None and "error" not in ref and r_code == 0}
    record = {
        "schema": SCHEMA, "version": VERSION, "module": module, "scene": scene,
        "era": module.split("/", 1)[0].lstrip("_"),
        "portal": {"exit": p_code, "seconds": p_seconds, "facts_sha": _digest(work / "portal.ndjson"),
                   "error": _last_error(p_err) if not ran["portal"] else None},
        "reference": {"exit": r_code, "seconds": r_seconds, "facts_sha": _digest(work / "reference.ndjson"),
                      "error": _last_error(r_err) if not ran["reference"] else None},
    }
    if ran["portal"] and ran["reference"]:
        record["outcome"] = "both"
        for tier, ignore in (("structure", sf.GEOMETRY_FACTS), ("geometry", frozenset())):
            result = sf.diff_subject(ref, portal, args.exclusions, ignore=ignore)
            record[tier] = {"verdict": result["verdict"], "first_difference": result["first_difference"],
                            "difference_count": result["difference_count"], "excluded": result["excluded"]}
        record["smoke"] = _smoke(ref_png, portal_png)
    else:
        record["outcome"] = {(True, False): "portal-only", (False, True): "reference-only"}.get(
            (ran["portal"], ran["reference"]), "neither")
        record["triage"] = triage(record)
    record["engines"] = args.engine_ids
    record["frames"] ={"reference": str(ref_png) if ref_png.exists() else None,
                        "portal": str(portal_png) if portal_png.exists() else None}
    return record


# Rule-based triage for scenes that ran in one engine only. First match wins; `side` is the
# engine whose error is matched. A scene no rule covers stays "untriaged".
TRIAGE = (
    ("reference", "Must specify either a file_name or svg_string SVGMobject", "reference-defect",
     "the pinned Reference cannot construct OldTex/OldTexText (Appendix C-18, BN-07)"),
    ("any", "ModuleNotFoundError", "environment", "a scene-side package is not installed"),
    ("any", r"not Found|No such file or directory|cannot read", "environment",
     "a private asset (image, SVG, sound, data) is absent"),
    ("reference", "unexpected keyword argument", "portal-leniency",
     "the Reference rejects a keyword the portal accepts"),
)


def triage(record):
    import re

    for side, pattern, label, reason in TRIAGE:
        sides = ("portal", "reference") if side == "any" else (side,)
        for name in sides:
            error = record[name].get("error") or ""
            if re.search(pattern, error):
                return {"label": label, "side": name, "rule": pattern, "reason": reason}
    return {"label": "untriaged"}


def _last_error(stderr):
    if stderr is None:
        return None
    lines = [line.strip() for line in stderr.splitlines() if "Error" in line or "Exception" in line
             or line.startswith("fmn-python:")]
    return lines[-1][:240] if lines else (stderr.strip().splitlines() or [""])[-1][:240]


def candidates(args):
    first, _, last = args.years.partition("-")
    scenes = enumerate_scenes(args.videos, range(int(first), int(last or first) + 1))
    if args.scene:
        chosen = {tuple(spec.rsplit(":", 1)) for spec in args.scene}
        return [s for s in scenes if s in chosen]
    if args.candidates:
        wanted = set(args.candidate_outcome.split(","))
        keep = {(r["module"], r["scene"]) for r in map(json.loads, args.candidates.read_text().splitlines())
                if r.get("outcome") in wanted}
        scenes = [s for s in scenes if s in keep]
    return scenes[: args.limit] if args.limit else scenes


def write_dashboard(records, path, title):
    """Outcome codes and counts only (the corpus is CC BY-NC-SA)."""
    both = [r for r in records if r["outcome"] == "both"]
    count = lambda rs, tier, verdict: sum(1 for r in rs if r[tier]["verdict"] == verdict)  # noqa: E731
    lines = [f"# {title}", "",
             "Generated by `scripts/corpus_differential.py` (fm-5wq.33): outcome codes only.", "",
             "| measure | scenes |", "|---|---|",
             f"| candidates | {len(records)} |",
             f"| ran in both engines | {len(both)} |"]
    for tier in ("structure", "geometry"):
        for verdict in ("equal", "equal-with-exclusions", "differs"):
            lines.append(f"| {tier}: {verdict} | {count(both, tier, verdict)} |")
    for outcome in ("reference-only", "portal-only", "neither"):
        lines.append(f"| {outcome} | {sum(1 for r in records if r['outcome'] == outcome)} |")
    triaged = {}
    for r in records:
        if "triage" in r:
            key = (r["outcome"], r["triage"]["label"], r["triage"].get("reason", ""))
            triaged[key] = triaged.get(key, 0) + 1
    lines += ["", "## One-engine outcomes by triage label", "",
              "| outcome | label | scenes | rule |", "|---|---|---|---|"]
    for (outcome, label, reason), n in sorted(triaged.items(), key=lambda kv: -kv[1]):
        lines.append(f"| {outcome} | {label} | {n} | {reason} |")
    clusters = {}
    for r in both:
        first = r["structure"]["first_difference"]
        if r["structure"]["verdict"] == "differs" and first:
            key = (first["class"] or "(scene)", first["fact"])
            clusters.setdefault(key, []).append(f"{r['module']}:{r['scene']}")
    lines += ["", "## Structure differences by first differing (class, fact)", "",
              "| class | fact | scenes | example |", "|---|---|---|---|"]
    for (cls, fact), scenes in sorted(clusters.items(), key=lambda kv: -len(kv[1])):
        lines.append(f"| {cls} | {fact} | {len(scenes)} | `{scenes[0]}` |")
    engines = sorted({r["engines"] for r in records if r.get("engines")})
    lines += ["", f"Engine identities: {', '.join(engines) or 'recorded per run'}.", ""]
    path.write_text("\n".join(lines), encoding="utf-8")


def write_gallery(records, path):
    rows = []
    for r in records:
        if r["outcome"] != "both":
            continue
        cells = "".join(f'<td><img src="{html.escape(os.path.relpath(r["frames"][side], path.parent))}" '
                        f'width="320"></td>' if r["frames"][side] else "<td>-</td>"
                        for side in ("reference", "portal"))
        smoke = r.get("smoke") or {}
        rows.append(f"<tr><td>{html.escape(r['module'])}<br>{html.escape(r['scene'])}<br>"
                    f"structure: {r['structure']['verdict']}<br>geometry: {r['geometry']['verdict']}<br>"
                    f"ssim {smoke.get('ssim')}</td>{cells}</tr>")
    path.write_text("<!doctype html><meta charset=utf-8><title>Corpus differential gallery</title>"
                    "<table border=1><tr><th>scene</th><th>Reference</th><th>FrankenManim</th></tr>"
                    + "".join(rows) + "</table>", encoding="utf-8")


def _construct_of(tree, scene):
    """The construct FunctionDef used by `scene`: its own, or the nearest in-module base's."""
    import ast

    classes = {node.name: node for node in tree.body if isinstance(node, ast.ClassDef)}
    queue, seen = [scene], set()
    while queue:
        name = queue.pop(0)
        if name in seen or name not in classes:
            continue
        seen.add(name)
        for item in classes[name].body:
            if isinstance(item, ast.FunctionDef) and item.name == "construct":
                return item
        queue.extend(base.id for base in classes[name].bases if isinstance(base, ast.Name))
    return None


def minimize(args, module, scene, target=None, budget=60):
    """Delta-debug `construct` (ddmin over its top-level statements) while the structure
    difference with the same (class, fact) as the full scene's first one persists.

    The reduced module is written under --out only (the corpus is CC BY-NC-SA). The record
    names the target difference, the statement counts, and the tests run.
    """
    import ast
    import math

    source = (args.videos / module).read_text(encoding="utf-8")
    tree = ast.parse(source)
    construct = _construct_of(tree, scene)
    if construct is None:
        return {"module": module, "scene": scene, "error": "no construct found in the module"}
    statements = list(construct.body)
    work = args.out / "minimize" / _slug(module, scene)
    work.mkdir(parents=True, exist_ok=True)
    tests = {"count": 0}

    def differences_for(kept):
        # Every variant stays under --out; nothing is written into, or removed from,
        # the corpus checkout. Corpus imports resolve through PYTHONPATH (the corpus root).
        construct.body = [statements[i] for i in kept] or [ast.Pass()]
        tests["count"] += 1
        run = work / f"run{tests['count']}"
        run.mkdir(exist_ok=True)
        variant = run / pathlib.Path(module).name
        variant.write_text(ast.unparse(tree), encoding="utf-8")
        run_portal(args, str(variant), scene, run)
        run_reference(args, str(variant), scene, run)
        ref, portal = _facts(run / "reference.ndjson"), _facts(run / "portal.ndjson")
        if ref is None or portal is None or "error" in ref or "error" in portal:
            return None
        return sf.diff_subject(ref, portal, args.exclusions, limit=200, ignore=sf.GEOMETRY_FACTS)

    full = differences_for(list(range(len(statements))))
    if full is None or full["verdict"] != "differs":
        return {"module": module, "scene": scene, "error": "the full scene does not differ structurally"}
    first = full["first_difference"]
    target = target or (first["class"], first["fact"])

    def reproduces(kept):
        if tests["count"] >= budget:
            return False
        result = differences_for(kept)
        return bool(result) and any((d["class"], d["fact"]) == tuple(target) for d in result["differences"])

    items, n = list(range(len(statements))), 2
    while len(items) >= 2 and tests["count"] < budget:
        size = math.ceil(len(items) / n)
        chunks = [items[i:i + size] for i in range(0, len(items), size)]
        for chunk in chunks:
            complement = [i for i in items if i not in chunk]
            if reproduces(complement):
                items, n = complement, max(n - 1, 2)
                break
        else:
            if n >= len(items):
                break
            n = min(len(items), 2 * n)
    construct.body = [statements[i] for i in items] or [ast.Pass()]
    (work / "minimized.py").write_text(ast.unparse(tree), encoding="utf-8")
    return {"module": module, "scene": scene, "target": list(target),
            "statements_before": len(statements), "statements_after": len(items),
            "tests_run": tests["count"], "budget_exhausted": tests["count"] >= budget,
            "minimized": str(work / "minimized.py")}


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--portal-python")
    parser.add_argument("--reference-python")
    parser.add_argument("--reference-root", type=pathlib.Path, default=ROOT / "scripts" / "manim_ref")
    parser.add_argument("--videos", type=pathlib.Path, default=ROOT / "scripts" / "videos_ref")
    parser.add_argument("--config", type=pathlib.Path)
    parser.add_argument("--out", type=pathlib.Path)
    parser.add_argument("--candidates", type=pathlib.Path)
    parser.add_argument("--candidate-outcome", default="ok")
    parser.add_argument("--years", default="2015-2026")
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--jobs", type=int, default=16)
    parser.add_argument("--timeout", type=int, default=240)
    parser.add_argument("--exclusions-file", type=pathlib.Path, default=EXCLUSIONS)
    parser.add_argument("--portal-id", default="unlabeled-portal",
                        help="the portal build identity recorded in every record (e.g. its wheel's commit)")
    parser.add_argument("--reference-id", default="3b1b/manim@6199a00d4c1b1127ebe45cb629c3f22538b10e13")
    parser.add_argument("--scene", action="append", default=[],
                        help="MODULE:SCENE to run (repeatable); overrides --candidates")
    parser.add_argument("--minimize", action="append", default=[],
                        help="MODULE:SCENE to delta-debug to a minimal structure-differing construct")
    parser.add_argument("--minimize-budget", type=int, default=60)
    parser.add_argument("--report", type=pathlib.Path, help="re-derive dashboard/gallery from DIR")
    parser.add_argument("--rediff", type=pathlib.Path,
                        help="recompute DIR's verdicts from its saved facts under the current exclusions")
    parser.add_argument("--dashboard", type=pathlib.Path)
    parser.add_argument("--title", default="Corpus differential")
    args = parser.parse_args()
    if args.rediff:
        # Recompute both tiers from the saved facts under the current exclusion table.
        exclusions = sf.in_scope(sf.load_exclusions(args.exclusions_file), "scenes")
        records = sf.read_ndjson(args.rediff / "records.ndjson")
        for r in records:
            if r["outcome"] != "both":
                continue
            work = args.rediff / "scenes" / _slug(r["module"], r["scene"])
            ref, portal = _facts(work / "reference.ndjson"), _facts(work / "portal.ndjson")
            for tier, ignore in (("structure", sf.GEOMETRY_FACTS), ("geometry", frozenset())):
                result = sf.diff_subject(ref, portal, exclusions, ignore=ignore)
                r[tier] = {"verdict": result["verdict"], "first_difference": result["first_difference"],
                           "difference_count": result["difference_count"], "excluded": result["excluded"]}
        (args.rediff / "records.ndjson").write_text("".join(sf.canonical(r) + "\n" for r in records))
        counts = {}
        for r in records:
            key = r["outcome"] if r["outcome"] != "both" else f"both:{r['structure']['verdict']}"
            counts[key] = counts.get(key, 0) + 1
        print(sf.canonical({"rediff": str(args.rediff), "counts": dict(sorted(counts.items()))}))
        return 0
    if args.report:
        records = sf.read_ndjson(args.report / "records.ndjson")
        if args.dashboard:
            write_dashboard(records, args.dashboard, args.title)
        write_gallery(records, args.report / "gallery.html")
        return 0
    if not (args.portal_python and args.reference_python and args.out):
        parser.error("a run requires --portal-python, --reference-python and --out")
    args.videos = args.videos.resolve()
    args.reference_root = args.reference_root.resolve()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    args.exclusions = sf.in_scope(sf.load_exclusions(args.exclusions_file), "scenes")
    args.engine_ids = f"reference={args.reference_id} portal={args.portal_id}"
    args.workdir = args.videos
    if args.config:
        args.workdir = args.out / "workdir"
        args.workdir.mkdir(exist_ok=True)
        (args.workdir / "custom_config.yml").write_text(args.config.read_text(encoding="utf-8"))
    if args.minimize:
        for spec in args.minimize:
            module, _, scene = spec.rpartition(":")
            print(sf.canonical(minimize(args, module, scene, budget=args.minimize_budget)))
        return 0
    scenes = candidates(args)
    print(f"{len(scenes)} candidate scenes", file=sys.stderr)
    records = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        futures = [pool.submit(run_one, args, module, scene) for module, scene in scenes]
        for done, future in enumerate(concurrent.futures.as_completed(futures), 1):
            records.append(future.result())
            if done % 25 == 0:
                print(f"{done}/{len(scenes)}", file=sys.stderr)
    # Merge into an earlier run in the same --out: re-run scenes replace their records.
    previous = args.out / "records.ndjson"
    if previous.exists():
        fresh = {(r["module"], r["scene"]) for r in records}
        records += [r for r in sf.read_ndjson(previous) if (r["module"], r["scene"]) not in fresh]
    records.sort(key=lambda r: (r["module"], r["scene"]))
    used = set()
    for r in records:
        if r["outcome"] == "both":
            used.update(r["geometry"]["excluded"])
    stale = sorted(row["id"] for row in args.exclusions if row["kind"] == "open-bead" and row["id"] not in used)
    (args.out / "records.ndjson").write_text("".join(sf.canonical(r) + "\n" for r in records))
    summary = {"schema": SCHEMA, "version": VERSION, "candidates": len(records),
               "engines": args.engine_ids, "outcomes": {}, "structure": {}, "geometry": {},
               "triage": {}, "stale_open_bead_exclusions": stale}
    for r in records:
        summary["outcomes"][r["outcome"]] = summary["outcomes"].get(r["outcome"], 0) + 1
        if "triage" in r:
            key = f"{r['outcome']}:{r['triage']['label']}"
            summary["triage"][key] = summary["triage"].get(key, 0) + 1
        if r["outcome"] == "both":
            for tier in ("structure", "geometry"):
                v = r[tier]["verdict"]
                summary[tier][v] = summary[tier].get(v, 0) + 1
    (args.out / "summary.json").write_text(sf.canonical(summary) + "\n")
    write_gallery(records, args.out / "gallery.html")
    print(sf.canonical(summary))
    return 0


if __name__ == "__main__":
    sys.exit(main())
