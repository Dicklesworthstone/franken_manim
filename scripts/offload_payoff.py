#!/usr/bin/env python3
"""The native-offload payoff (fm-offload-payoff-4rve): segment census plus prediction.

Input: `corpus_speed_ratio.py --frames --census` run directories. A runs.ndjson pairs each
scene's portal and Reference wall times, and each portal run left the segment census the portal
records under FMN_SEGMENT_CENSUS (fmn_python/segment_census.py, schema fmn.segment-census v1).
A later `--runs` directory re-measures scenes (say, the first run's timeouts at a longer
timeout): its records replace every earlier record of the same scene, both engines.
Also: the same-scene native/portal measurements (`--same-scene`, runs.json files of
native-vs-portal pairs on one scene).

The prediction, per scene measured in both engines:

    offloaded portal time = time outside segments + r x (native + rate-table segment time)
                            + time of the python segments
    predicted ratio       = offloaded portal time / Reference time

r is the median native/portal wall ratio on identical work. "Strict" keeps every python segment
in Python. "Library updaters native" also offloads the python segments whose only reason is an
updater the library itself defines (InteractiveScene's selection highlight). The report states
both, with the PG-1 bars (<= 0.50x at G2, <= 0.35x at G4).

Usage: offload_payoff.py --runs DIR [--runs RERUN_DIR]... --portal-scenes N --same-scene FILE[:LABEL]...
                         --ratio-from LABEL --out docs/ratchet/offload_payoff.md
"""
from __future__ import annotations

import argparse
import datetime
import json
import pathlib
import statistics
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from corpus_speed_ratio import bootstrap_interval, load_runs, percentile, scene_ratios  # noqa: E402

CLASSES = ("native", "rate-table", "python")


def slug(module: str, scene: str) -> str:
    return f"{module.replace('/', '__')}__{scene}"


def load_census(run_dir: pathlib.Path, module: str, scene: str) -> list[dict]:
    path = run_dir / "work" / slug(module, scene) / "census-0.ndjson"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line]


def merged_runs(run_dirs: list[pathlib.Path]) -> tuple[list[dict], dict]:
    """Run records across `run_dirs`, and the directory each scene's records come from: a later
    directory's records replace every earlier record of the same scene."""
    runs, owner = [], {}
    for run_dir in run_dirs:
        records = load_runs(run_dir / "runs.ndjson")
        scenes = {(r["module"], r["scene"]) for r in records}
        runs = [r for r in runs if (r["module"], r["scene"]) not in scenes] + records
        owner.update((scene, run_dir) for scene in scenes)
    return runs, owner


def library_only(segment: dict) -> bool:
    reasons = segment["reasons"]
    return bool(reasons) and all(reason.startswith("library-updaters:") for reason in reasons)


def predict(portal_s: float, reference_s: float, segments: list[dict], r: float) -> dict:
    by_class = {klass: sum(s["wall_s"] for s in segments if s["class"] == klass)
                for klass in CLASSES}
    library = sum(s["wall_s"] for s in segments if s["class"] == "python" and library_only(s))
    outside = max(0.0, portal_s - sum(by_class.values()))
    offloadable = by_class["native"] + by_class["rate-table"]
    strict = outside + r * offloadable + by_class["python"]
    optimistic = outside + r * (offloadable + library) + by_class["python"] - library
    return {"current": portal_s / reference_s, "strict": strict / reference_s,
            "optimistic": optimistic / reference_s}


def same_scene(files: list[str]) -> list[dict]:
    rows = []
    for spec in files:
        path, _, label = spec.partition(":")
        runs = json.loads(pathlib.Path(path).read_text(encoding="utf-8"))
        medians = {}
        for engine in ("native", "portal"):
            ok = [r for r in runs if r["engine"] == engine and r["exit"] == 0]
            medians[engine] = (statistics.median(r["wall_s"] for r in ok),
                               statistics.median(r["cpu_s"] for r in ok), len(ok),
                               statistics.median(r["load1"] for r in ok))
        rows.append({"label": label or path, "native": medians["native"],
                     "portal": medians["portal"],
                     "ratio": medians["native"][0] / medians["portal"][0]})
    return rows


def write_report(args) -> dict:
    header = json.loads((args.runs[0] / "header.json").read_text(encoding="utf-8"))
    runs, owner = merged_runs(args.runs)
    portal_ok = sorted({(r["module"], r["scene"]) for r in runs
                        if r["engine"] == "portal" and r["exit"] == 0})
    censuses = {scene: load_census(owner[scene], *scene) for scene in portal_ok}
    segments = [s for records in censuses.values() for s in records]
    pairs = same_scene(args.same_scene)
    r = next(row["ratio"] for row in pairs if row["label"] == args.ratio_from)
    included, excluded = scene_ratios(runs)
    predictions = {scene: predict(row["portal_s"], row["reference_s"], censuses.get(scene, []), r)
                   for scene, row in included.items() if scene in censuses}

    def summary(key):
        values = [p[key] for p in predictions.values()]
        median = statistics.median(values)
        low, high = bootstrap_interval(values, statistics.median)
        return (f"{median:.2f} ({low:.2f}-{high:.2f})", f"{percentile(values, 90):.2f}",
                sum(v <= 0.50 for v in values), sum(v <= 0.35 for v in values))

    commit = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "--short", "HEAD"],
                            capture_output=True, text=True, timeout=30).stdout.strip()
    total = sum(s["wall_s"] for s in segments) or 1.0
    lines = [
        "# Native-offload payoff: segment census and prediction",
        "",
        "**calibration-unqualified (ADR-0024).** Generated by `scripts/offload_payoff.py`"
        f" (fm-offload-payoff-4rve) on {datetime.date.today().isoformat()} at `{commit}`, from a"
        " `scripts/corpus_speed_ratio.py --frames --census` run. It is the measurement behind the"
        " owner's ADR-0026 ruling (D3), not PG-1 evidence: the host was shared and loaded.",
        "",
        "| run fact | value |",
        "|---|---|",
    ]
    for key in ("cpu", "logical_cpus", "platform", "portal_id", "reference_id", "mode", "reps",
                "timeout_s"):
        lines.append(f"| {key} | {header.get(key, '')} |")
    for run_dir in args.runs[1:]:
        rerun = json.loads((run_dir / "header.json").read_text(encoding="utf-8"))
        differ = [key for key in ("portal_id", "reference_id", "mode", "reps")
                  if rerun.get(key) != header.get(key)]
        if differ:
            sys.exit(f"{run_dir} re-measures with a different {', '.join(differ)}")
        count = sum(1 for d in owner.values() if d == run_dir)
        lines.append(f"| re-measured | {count} scenes at timeout_s {rerun.get('timeout_s', '')} |")
    lines += [
        "",
        "## Census",
        "",
        f"{len(portal_ok)} of the {args.portal_scenes} selected corpus scenes ran"
        f" under the census ({100 * len(portal_ok) / args.portal_scenes:.1f}%). Scenes among"
        f" them that play no segment at all: {sum(1 for c in censuses.values() if not c)}.",
        "",
        "| class | segments | wall s | share of segment time |",
        "|---|---:|---:|---:|",
    ]
    for klass in CLASSES:
        chosen = [s for s in segments if s["class"] == klass]
        wall = sum(s["wall_s"] for s in chosen)
        lines.append(f"| {klass} | {len(chosen)} | {wall:.1f} | {100 * wall / total:.1f}% |")
    reasons = {}
    for s in segments:
        for reason in s["reasons"]:
            name = reason.split(":")[0] + (":" + reason.split(":")[1]
                                           if reason.startswith("python-animation:") else "")
            count, wall = reasons.get(name, (0, 0.0))
            reasons[name] = (count + 1, wall + s["wall_s"])
    lines += ["", "Why python segments are python (a segment can have several reasons; by the"
              " wall time of the segments carrying each):", "",
              "| reason | segments | wall s |", "|---|---:|---:|"]
    for name, (count, wall) in sorted(reasons.items(), key=lambda item: -item[1][1])[:25]:
        lines.append(f"| `{name}` | {count} | {wall:.1f} |")
    lines += [
        "",
        "## Same-scene native vs portal",
        "",
        "Portal `SquareToCircle` against `crates/fmn/examples/square_to_circle.rs`, 640x360 at 30"
        " fps, PNG sequence, interleaved. Medians.",
        "",
        "| measurement | native wall s | native CPU s | portal wall s | portal CPU s |"
        " native/portal wall | runs | load (1 min) |",
        "|---|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for row in pairs:
        n, p = row["native"], row["portal"]
        lines.append(f"| {row['label']} | {n[0]:.2f} | {n[1]:.2f} | {p[0]:.2f} | {p[1]:.2f} |"
                     f" {row['ratio']:.2f} | {n[2]}+{p[2]} | {n[3]:.0f}/{p[3]:.0f} |")
    current, strict, optimistic = summary("current"), summary("strict"), summary("optimistic")
    lines += [
        "",
        f"The prediction uses r = {r:.2f} (`{args.ratio_from}`).",
        "",
        "## Prediction",
        "",
        f"{len(predictions)} scenes ran in both engines ({len(excluded)} more failed or timed out"
        " on a side and are left out).",
        "",
        "| measure | portal today | offloaded, strict | offloaded, library updaters native |",
        "|---|---:|---:|---:|",
        f"| portal/Reference ratio, median (95% CI) | {current[0]} | {strict[0]} | {optimistic[0]} |",
        f"| p90 | {current[1]} | {strict[1]} | {optimistic[1]} |",
        f"| scenes at <= 0.50x (PG-1, G2) | {current[2]} | {strict[2]} | {optimistic[2]} |",
        f"| scenes at <= 0.35x (PG-1, G4) | {current[3]} | {strict[3]} | {optimistic[3]} |",
        "",
        "## Method and limits",
        "",
        "- A segment is one play() or wait(). Its class comes from what the portal itself routes:"
        " a Python callback animation, an updater (authored, or defined by the library), an"
        " authored scene or mobject hook, or a stop condition makes it `python`. A custom rate"
        " function on a shipped animation makes it `rate-table`, which offload can tabulate.",
        "- Time outside segments (process start, imports, construction, encoding at the end) is"
        " kept as measured.",
        "- r comes from one scene. Raster cost shrinks at the census's lower resolution while the"
        " portal's per-frame Python cost does not, so r measured at 640x360 likely understates"
        " the benefit at 320x180.",
        "- One repetition per scene, on a host shared with other work. The census run's portal ran"
        " on one render thread, so the prediction leaves out frame parallelism.",
    ]
    args.out.parent.mkdir(parents=True, exist_ok=True)
    from check_measurement_staleness import producer_header
    measured = getattr(args, "measured_commit", None) or commit
    inputs = sorted({(row["module"], row["scene"]) for row in runs})
    lines.insert(0, producer_header(measured, [f"{m}:{s}" for m, s in inputs]))
    args.out.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return {"census_scenes": len(portal_ok), "segments": len(segments),
            "predicted_scenes": len(predictions), "r": r, "current": current[0],
            "strict": strict[0], "optimistic": optimistic[0]}


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--runs", type=pathlib.Path, action="append", required=True,
                        help="a census run directory; repeat for re-measurements, later wins")
    parser.add_argument("--portal-scenes", type=int, required=True,
                        help="original selected corpus cohort size (the coverage denominator)")
    parser.add_argument("--same-scene", action="append", default=[],
                        help="runs.json of a native-vs-portal pairing, optionally :LABEL")
    parser.add_argument("--ratio-from", required=True, help="the same-scene LABEL whose r to use")
    parser.add_argument("--out", type=pathlib.Path, default=ROOT / "docs" / "ratchet" / "offload_payoff.md")
    parser.add_argument("--measured-commit",
                        help="the commit the measured wheel was built from (default: HEAD); stamps"
                             " the report's staleness header (check_measurement_staleness.py)")
    print(json.dumps(write_report(parser.parse_args(argv))))
    return 0


if __name__ == "__main__":
    sys.exit(main())
