#!/usr/bin/env python3
"""Calibration-mode corpus speed ratio: portal vs pinned Reference wall time (fm-5wq.35).

For corpus scenes that run in both engines, time the portal and the pinned
Reference on the same host. Runs are interleaved per scene (portal,
reference, portal, reference, ...) for --reps repetitions, in final-state
mode (-s) at 320x180, with one workdir and config. Each run is one NDJSON
line in OUT/runs.ndjson. The file is resumable: a finished
(scene, engine, rep) triple is never rerun, so a long corpus run can be
extended over sessions.

`--report` derives:
- per scene, the median of the paired ratios portal/reference over complete
  repetitions, with their min and max;
- for the corpus, the distribution of scene ratios (median, p90, p99) with
  seeded bootstrap 95% intervals;
- the markdown table.
A scene whose portal or Reference run fails or times out is excluded and
counted, never imputed.

Everything here is calibration mode and UNQUALIFIED (ADR-0024): a shared,
loaded dev host, never PG-1 evidence. The corpus is CC BY-NC-SA, so records
carry scene identifiers and timings only, never scene source.

Measurement notes:
- The Reference renders under one persistent Xvfb server started here, so
  per-run xvfb-run startup is not billed to it.
- Both engines run skip mode, and skip mode differs by design (BN-10): the
  portal runs every frame's updaters, while the Reference collapses a skipped
  segment into one step. For updater-heavy scenes the ratio therefore
  measures that deliberate semantic difference as well as speed (see
  fm-5wq.31).

Usage:
    corpus_speed_ratio.py --portal-python P --reference-python R --out DIR \
        [--candidates published_ok.ndjson | --scene MODULE:SCENE ...] \
        [--sample N --sample-seed S] [--reps 3] [--timeout 300] [--jobs 4]
    corpus_speed_ratio.py --report DIR [--dashboard docs/ratchet/speed_ratio.md]
"""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import platform
import random
import signal
import statistics
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from corpus_scene_sweep import run_child  # noqa: E402

SCHEMA = "fmn.speed-ratio"
VERSION = 1
LABEL = "calibration-unqualified (ADR-0024)"
ENGINES = ("portal", "reference")
BOOTSTRAP_RESAMPLES = 2000
BOOTSTRAP_SEED = 20260928


# ----------------------------------------------------------------- planning

def interleaved_plan(scenes, reps, engines=ENGINES):
    """(scene, rep, engine) in run order: per scene, engines alternate A B A B."""
    return [(scene, rep, engine) for scene in scenes for rep in range(reps) for engine in engines]


def load_runs(path: pathlib.Path):
    """Run records from a runs.ndjson; `#` lines and blanks are skipped.

    A producer killed mid-append leaves an unterminated last line; that one
    record is dropped (its run reruns on resume). Any other malformed line
    raises.
    """
    if not path.exists():
        return []
    text = path.read_text(encoding="utf-8")
    lines = text.split("\n")
    if lines[-1].strip():
        lines.pop()
    return [json.loads(line) for line in lines if line.strip() and not line.startswith("#")]


# What a resumed measurement must share with the one that started the log.
IDENTITY = ("portal_id", "reference_id", "reps", "timeout_s", "mode", "schema", "version")


def resume_conflicts(old_header, new_header):
    """The identity fields on which a resume would mix incomparable runs."""
    return [field for field in IDENTITY if old_header.get(field) != new_header.get(field)]


def run_key(record):
    return (record["module"], record["scene"], record["engine"], record["rep"])


# ----------------------------------------------------------------- measuring

def measure_scene(scene, reps, runners, log, done, lock, engines=ENGINES):
    """Run one scene's interleaved plan with `runners[engine](scene, rep)`.

    A runner returns the child's exit code, or None on timeout. It is timed
    here, around the call, on the monotonic clock. After the first failure on
    either side the scene's remaining repetitions are not run: the scene is
    already excluded from the ratio, and a timeout per repetition would only
    burn the budget.
    """
    module, name = scene
    for _, rep, engine in interleaved_plan([scene], reps, engines):
        key = (module, name, engine, rep)
        if key in done:
            continue
        load_start = os.getloadavg()[0]
        start = time.monotonic()
        code = runners[engine](scene, rep)
        seconds = time.monotonic() - start
        record = {"schema": SCHEMA, "version": VERSION, "label": LABEL, "module": module,
                  "scene": name, "engine": engine, "rep": rep, "exit": code,
                  "seconds": round(seconds, 4), "load_start": round(load_start, 2),
                  "load_end": round(os.getloadavg()[0], 2)}
        with lock:
            log(record)
            done.add(key)
        if code != 0:
            return False
    return True


# ------------------------------------------------------------------- stats

def percentile(values, q):
    """Linear-interpolation percentile (numpy's default) of a non-empty list, q in [0, 100]."""
    ordered = sorted(values)
    if not ordered:
        raise ValueError("percentile of an empty list")
    position = (len(ordered) - 1) * q / 100.0
    low = int(position)
    high = min(low + 1, len(ordered) - 1)
    return ordered[low] + (ordered[high] - ordered[low]) * (position - low)


def bootstrap_interval(values, statistic, resamples=BOOTSTRAP_RESAMPLES, seed=BOOTSTRAP_SEED):
    """Seeded percentile-bootstrap 95% interval of `statistic` over `values`."""
    rng = random.Random(seed)
    count = len(values)
    estimates = [statistic([values[rng.randrange(count)] for _ in range(count)])
                 for _ in range(resamples)]
    return percentile(estimates, 2.5), percentile(estimates, 97.5)


def scene_ratios(runs):
    """Per-scene ratio summaries and the excluded scenes, from run records.

    A repetition counts when both engines exited 0 in it. The scene ratio is
    the median of those paired portal/reference ratios. A scene with no
    complete repetition is excluded and its reason recorded: the side(s)
    that failed or timed out, or a side never run.
    """
    by_scene = {}
    for record in runs:
        by_scene.setdefault((record["module"], record["scene"]), {})[
            (record["engine"], record["rep"])] = record
    included, excluded = {}, {}
    for scene, cells in sorted(by_scene.items()):
        reps = sorted({rep for _, rep in cells})
        pairs = []
        for rep in reps:
            portal, reference = cells.get(("portal", rep)), cells.get(("reference", rep))
            if portal and reference and portal["exit"] == 0 and reference["exit"] == 0 \
                    and reference["seconds"] > 0:
                pairs.append((portal["seconds"], reference["seconds"]))
        if not pairs:
            failed = sorted({record["engine"] + (":timeout" if record["exit"] is None else
                                                 f":exit{record['exit']}")
                             for record in cells.values() if record["exit"] != 0})
            excluded[scene] = failed or ["incomplete"]
            continue
        ratios = [portal / reference for portal, reference in pairs]
        included[scene] = {
            "ratio": statistics.median(ratios),
            "min": min(ratios),
            "max": max(ratios),
            "pairs": len(pairs),
            "portal_s": statistics.median(p for p, _ in pairs),
            "reference_s": statistics.median(r for _, r in pairs),
        }
    return included, excluded


def corpus_summary(included):
    ratios = [row["ratio"] for row in included.values()]
    if not ratios:
        return {"scenes": 0}
    summary = {"scenes": len(ratios)}
    for name, q in (("median", 50), ("p90", 90), ("p99", 99)):
        summary[name] = percentile(ratios, q)
        summary[name + "_ci95"] = bootstrap_interval(ratios, lambda sample, q=q: percentile(sample, q))
    summary["above_1x"] = sum(ratio > 1.0 for ratio in ratios)
    return summary


# ------------------------------------------------------------------ report

def write_dashboard(out_dir: pathlib.Path, dashboard: pathlib.Path):
    runs = load_runs(out_dir / "runs.ndjson")
    header = json.loads((out_dir / "header.json").read_text(encoding="utf-8")) \
        if (out_dir / "header.json").exists() else {}
    included, excluded = scene_ratios(runs)
    summary = corpus_summary(included)
    lines = [
        "# Corpus speed ratio: portal vs pinned Reference",
        "",
        f"**{LABEL}.** Generated by `scripts/corpus_speed_ratio.py` (fm-5wq.35, schema"
        f" `{SCHEMA}` v{VERSION}). Ratio = portal wall time / Reference wall time, paired per"
        " interleaved repetition, median per scene. Below 1 means the portal is faster. Not PG-1"
        " evidence. Skip mode differs by design (BN-10): the portal runs every frame's"
        " updaters, the Reference one step per skipped segment.",
        "",
        "| host fact | value |",
        "|---|---|",
    ]
    for key in ("cpu", "logical_cpus", "governor", "platform", "portal_id", "reference_id",
                "reps", "timeout_s", "mode"):
        if key in header:
            lines.append(f"| {key} | {header[key]} |")
    lines += ["", "| measure | value |", "|---|---|",
              f"| scenes measured in both engines | {summary['scenes']} |",
              f"| scenes excluded (a side failed or timed out) | {len(excluded)} |"]
    if summary["scenes"]:
        for name in ("median", "p90", "p99"):
            low, high = summary[name + "_ci95"]
            lines.append(f"| ratio {name} (95% bootstrap CI) | {summary[name]:.2f}"
                         f" ({low:.2f}-{high:.2f}) |")
        lines.append(f"| scenes above 1x | {summary['above_1x']} |")
    lines += ["", "## Per scene (slowest first)", "",
              "| scene | ratio | min | max | pairs | portal s | Reference s |",
              "|---|---:|---:|---:|---:|---:|---:|"]
    for (module, name), row in sorted(included.items(), key=lambda item: -item[1]["ratio"]):
        lines.append(f"| {module}:{name} | {row['ratio']:.2f} | {row['min']:.2f} |"
                     f" {row['max']:.2f} | {row['pairs']} | {row['portal_s']:.1f} |"
                     f" {row['reference_s']:.1f} |")
    if excluded:
        lines += ["", "## Excluded (never imputed)", "", "| scene | failed side |", "|---|---|"]
        for (module, name), reasons in excluded.items():
            lines.append(f"| {module}:{name} | {', '.join(reasons)} |")
    dashboard.parent.mkdir(parents=True, exist_ok=True)
    dashboard.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return summary, len(excluded)


# ------------------------------------------------------------------ engines

def host_facts():
    facts = {"logical_cpus": os.cpu_count(), "platform": platform.platform()}
    try:
        for line in pathlib.Path("/proc/cpuinfo").read_text(encoding="utf-8").splitlines():
            if line.startswith("model name"):
                facts["cpu"] = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    try:
        facts["governor"] = pathlib.Path(
            "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor").read_text().strip()
    except OSError:
        pass
    return facts


class XvfbServer:
    """One Xvfb display for every Reference run of this producer."""

    def __init__(self):
        self.proc, self.display = None, None

    def __enter__(self):
        for number in range(4200, 4300):
            proc = subprocess.Popen(["Xvfb", f":{number}", "-screen", "0", "1920x1080x24",
                                     "-nolisten", "tcp"], stdout=subprocess.DEVNULL,
                                    stderr=subprocess.DEVNULL, start_new_session=True)
            time.sleep(1.0)
            if proc.poll() is None:
                self.proc, self.display = proc, f":{number}"
                return self
        raise RuntimeError("no free Xvfb display in :4200-:4299")

    def __exit__(self, *exc):
        if self.proc is not None:
            os.killpg(self.proc.pid, signal.SIGTERM)
            self.proc.wait()


def make_runners(args, display):
    work = args.out / "work"

    def portal(scene, rep):
        module, name = scene
        target = work / f"{module.replace('/', '__')}__{name}" / f"portal-{rep}.png"
        target.parent.mkdir(parents=True, exist_ok=True)
        env = dict(os.environ, PYTHONPATH=str(args.videos))
        argv = [args.portal_python, "-m", "fmn_python", str(args.videos / module), name, "-s",
                "--format", "png", "--resolution", "320x180", "--video_dir", str(target)]
        return run_child(argv, args.workdir, env, args.timeout)[0]

    def reference(scene, rep):
        module, name = scene
        target = work / f"{module.replace('/', '__')}__{name}" / f"reference-{rep}"
        target.mkdir(parents=True, exist_ok=True)
        env = {"PATH": "/usr/bin:/bin", "HOME": os.environ.get("HOME", "/tmp"),
               "DISPLAY": display,
               "PYTHONPATH": f"{args.reference_root}{os.pathsep}{args.videos}"}
        argv = [args.reference_python, "-m", "manimlib", str(args.videos / module), name, "-s",
                "-w", "-r", "320x180", "--video_dir", str(target)]
        return run_child(argv, args.workdir, env, args.timeout)[0]

    return {"portal": portal, "reference": reference}


def select_scenes(args):
    if args.scene:
        return [tuple(item.split(":", 1)) for item in args.scene]
    scenes = []
    for line in args.candidates.read_text(encoding="utf-8").splitlines():
        if line.strip():
            record = json.loads(line)
            if record.get("outcome", "ok") == "ok":
                scenes.append((record["module"], record["scene"]))
    scenes = sorted(set(scenes))
    if args.sample:
        scenes = sorted(random.Random(args.sample_seed).sample(scenes, min(args.sample, len(scenes))))
    return scenes


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--portal-python")
    parser.add_argument("--reference-python")
    parser.add_argument("--reference-root", type=pathlib.Path, default=ROOT / "scripts" / "manim_ref")
    parser.add_argument("--videos", type=pathlib.Path, default=ROOT / "scripts" / "videos_ref")
    parser.add_argument("--config", type=pathlib.Path, help="custom_config.yml for the shared workdir")
    parser.add_argument("--out", type=pathlib.Path)
    parser.add_argument("--candidates", type=pathlib.Path, help="NDJSON of {module, scene, outcome}")
    parser.add_argument("--scene", action="append", default=[], help="MODULE:SCENE (repeatable)")
    parser.add_argument("--sample", type=int, default=0)
    parser.add_argument("--sample-seed", type=int, default=35)
    parser.add_argument("--reps", type=int, default=3)
    parser.add_argument("--timeout", type=int, default=300)
    parser.add_argument("--jobs", type=int, default=1)
    parser.add_argument("--portal-id", default="unlabeled-portal")
    parser.add_argument("--reference-id", default="3b1b/manim@6199a00d4c1b1127ebe45cb629c3f22538b10e13")
    parser.add_argument("--report", type=pathlib.Path, help="derive the dashboard from DIR")
    parser.add_argument("--dashboard", type=pathlib.Path)
    args = parser.parse_args()

    if args.report:
        dashboard = args.dashboard or args.report / "speed_ratio.md"
        summary, excluded = write_dashboard(args.report, dashboard)
        print(json.dumps({"schema": SCHEMA, "summary": summary, "excluded": excluded,
                          "dashboard": str(dashboard)}, default=list))
        return 0
    if not (args.portal_python and args.reference_python and args.out):
        parser.error("--portal-python, --reference-python and --out are required to measure")
    args.out.mkdir(parents=True, exist_ok=True)
    args.workdir = args.out / "workdir"
    args.workdir.mkdir(exist_ok=True)
    if args.config:
        (args.workdir / "custom_config.yml").write_text(args.config.read_text(encoding="utf-8"),
                                                        encoding="utf-8")
    header = dict(host_facts(), portal_id=args.portal_id, reference_id=args.reference_id,
                  reps=args.reps, timeout_s=args.timeout, mode="final-state -s, 320x180",
                  label=LABEL, schema=SCHEMA, version=VERSION)
    header_path, log_path = args.out / "header.json", args.out / "runs.ndjson"
    if log_path.exists() and header_path.exists():
        conflicts = resume_conflicts(json.loads(header_path.read_text(encoding="utf-8")), header)
        if conflicts:
            parser.error(f"{args.out} was started with different {', '.join(conflicts)};"
                         " resume with the same settings or use a new --out")
    header_path.write_text(json.dumps(header, indent=1) + "\n", encoding="utf-8")
    done = {run_key(record) for record in load_runs(log_path)}
    lock = threading.Lock()
    handle = log_path.open("a", encoding="utf-8")

    def log(record):
        handle.write(json.dumps(record, sort_keys=True) + "\n")
        handle.flush()

    scenes = select_scenes(args)
    with XvfbServer() as xvfb:
        runners = make_runners(args, xvfb.display)
        with ThreadPoolExecutor(max_workers=max(1, args.jobs)) as pool:
            list(pool.map(lambda scene: measure_scene(scene, args.reps, runners, log, done, lock),
                          scenes))
    handle.close()
    print(json.dumps({"schema": SCHEMA, "scenes": len(scenes), "runs": str(log_path)}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
