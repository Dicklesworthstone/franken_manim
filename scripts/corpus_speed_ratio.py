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
  fm-5wq.31). --reference-frame-grid removes it: the Reference then steps
  skipped segments on the frame grid (still without capture), through two
  Scene methods replaced in-process (REFERENCE_FRAME_GRID), so both engines
  do the same work. The mode is part of the run identity.

The calibration lane (fm-perf-calibration-lane-pkn8) answers single-timing
noise on a loaded host:
- Pairs run in ABBA order (portal first, then Reference first, alternating),
  so a load trend within a pair cancels over two pairs.
- Each pair gives a log-ratio log(t_portal / t_reference). A scene reports
  their Hodges-Lehmann estimate, the median of the Walsh averages, with the
  exact Wilcoxon signed-rank interval. That interval is valid at a fixed
  number of pairs.
- --max-pairs N runs a scene until a confidence sequence for the median
  log-ratio decides each of 0.5x and 1.0x: the ratio is below the threshold,
  above it, or within 5% of it. The sequence is the running intersection of
  {m : sign-test e-value(m) < 1/alpha}, a beta-binomial mixture over the
  signs of x - m, so by Ville's inequality its coverage holds however often
  it is looked at. Clear answers stop early (from 7 pairs at alpha 0.05);
  close ones run to N.
- --instructions counts retired instructions. `perf` wraps every timed run
  when the host allows perf events, and is recorded unavailable otherwise.
  `cachegrind` adds one warm valgrind run per engine after the timed pairs:
  single-threaded (portal --threads 1, llvmpipe and BLAS at one thread),
  PYTHONHASHSEED=0, children included. It is about 30x slower, so it is
  never timed. Single-threaded counts do not move with load (0.004% at load
  16), which makes them the regression signal on a busy host. Multithreaded
  counts do move: waiting workers spin.
Every record carries qualification=calibration-unqualified. These rows are
never PG gate observations (ADR-0024).

Usage:
    corpus_speed_ratio.py --portal-python P --reference-python R --out DIR \
        [--candidates published_ok.ndjson | --scene MODULE:SCENE ...] \
        [--sample N --sample-seed S] [--reps 3] [--timeout 300] [--jobs 4] \
        [--max-pairs 40] [--instructions none|perf|cachegrind]
    corpus_speed_ratio.py --report DIR [--dashboard docs/ratchet/speed_ratio.md]
"""
from __future__ import annotations

import argparse
import json
import math
import os
import pathlib
import platform
import random
import re
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
QUALIFICATION = "calibration-unqualified"
ENGINES = ("portal", "reference")
ORDER = "ABBA"
# The sequential lane decides each threshold: PG-1's 0.5x bar and parity.
DECISION_THRESHOLDS = (0.5, 1.0)
DECISION_TOLERANCE = 1.05  # "at the threshold": the whole sequence within +-5%
CS_ALPHA = 0.05
CS_PRIOR = 0.5  # Beta(1/2, 1/2) mixture over the sign probability (Krichevsky-Trofimov)
BOOTSTRAP_RESAMPLES = 2000
BOOTSTRAP_SEED = 20260928
SKIP_MODE = "final-state -s, 320x180"
FRAME_GRID_MODE = SKIP_MODE + ", Reference stepped on the frame grid (BN-10 work-matched)"
# --frames: every frame rendered and encoded to MP4 by both engines, through ffmpeg. The portal's
# ffmpeg boundary refuses a group- or other-writable output ancestor, so run under umask 022.
FRAMES_MODE = "every frame rendered to MP4, 320x180 at 15 fps (portal on one render thread)"
FRAMES_FPS = "15"
# `python -c` program for --reference-frame-grid. The pinned Reference's
# source is not edited; two Scene methods are replaced in-process before its
# own entry point runs.
REFERENCE_FRAME_GRID = '''\
"""The pinned Reference with skipped segments stepped on the frame grid.

Under -s the Reference advances a skipped segment in one step, then runs a
second full-duration finish pass. The portal (BN-10) steps every frame
without capturing. This keeps -s (no capture, no emit) and replaces only
those two Scene methods, so both engines do the same updater and
interpolation work.
"""
import sys

from manimlib.scene.scene import Scene

_progression = Scene.get_time_progression


def get_time_progression(self, run_time, n_iterations=None, desc="", override_skip_animations=False):
    return _progression(self, run_time, n_iterations, desc, override_skip_animations=True)


def finish_animations(self, animations):
    for animation in animations:
        animation.finish()
        animation.clean_up_from_scene(self)
    self.update_mobjects(0)


Scene.get_time_progression = get_time_progression
Scene.finish_animations = finish_animations
sys.argv = ["manimlib", *sys.argv[1:]]
from manimlib.__main__ import main  # noqa: E402

main()
'''


# ----------------------------------------------------------------- planning

def pair_order(rep, engines=ENGINES):
    """The engines of repetition `rep` in run order: A B, then B A (ABBA)."""
    return tuple(engines) if rep % 2 == 0 else tuple(reversed(engines))


def interleaved_plan(scenes, reps, engines=ENGINES):
    """(scene, rep, engine) in run order: per scene, pairs alternate A B, B A (ABBA)."""
    return [(scene, rep, engine) for scene in scenes for rep in range(reps)
            for engine in pair_order(rep, engines)]


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
IDENTITY = ("portal_id", "reference_id", "reps", "timeout_s", "mode", "schema", "version",
            "order", "max_pairs", "instructions")


def resume_conflicts(old_header, new_header):
    """The identity fields on which a resume would mix incomparable runs."""
    return [field for field in IDENTITY if old_header.get(field) != new_header.get(field)]


def run_key(record):
    return (record["module"], record["scene"], record["engine"], record["rep"])


# ----------------------------------------------------------------- measuring

def measure_scene(scene, reps, runners, log, done, lock, engines=ENGINES, max_pairs=None,
                  times=None):
    """Run one scene's pairs, in ABBA order, with `runners[engine](scene, rep)`.

    A runner returns (exit code or None on timeout, stderr text), optionally with a
    dict of extra record fields (instruction counts). It is timed here, around the call,
    on the monotonic clock. A failed run keeps its last stderr line as `error`, so an
    exclusion carries its cause. A failure on either side still completes its
    repetition's pair, so an excluded scene records the other engine's time (a portal
    timeout next to the Reference's seconds is the worst case's lower bound). The
    remaining repetitions are not run, since a timeout per repetition would only burn the
    budget.

    Without `max_pairs` the scene runs `reps` pairs. With it, the scene runs at least
    `reps` pairs. It stops as soon as the confidence sequence over its log-ratios decides
    every threshold (`sequential_decision`), or at `max_pairs`. `times` maps a successful
    run's key to its seconds, for resumed runs as for new ones.
    """
    module, name = scene
    times = {} if times is None else times
    limit = reps if max_pairs is None else max(reps, max_pairs)
    rep = 0
    while rep < limit:
        order = "AB" if pair_order(rep, engines) == tuple(engines) else "BA"
        failed = False
        for engine in pair_order(rep, engines):
            key = (module, name, engine, rep)
            if key in done:
                failed = failed or done[key] != 0  # a logged failure: finish its pair, then stop
                continue
            load_start = os.getloadavg()[0]
            start = time.monotonic()
            result = runners[engine](scene, rep)
            seconds = time.monotonic() - start
            code, stderr = result[0], result[1]
            record = {"schema": SCHEMA, "version": VERSION, "label": LABEL,
                      "qualification": QUALIFICATION, "module": module, "scene": name,
                      "engine": engine, "rep": rep, "order": order, "exit": code,
                      "seconds": round(seconds, 4), "load_start": round(load_start, 2),
                      "load_end": round(os.getloadavg()[0], 2)}
            if len(result) > 2:
                record.update(result[2])
            if code != 0:
                record["error"] = last_line(stderr)
            with lock:
                log(record)
                done[key] = code
                if code == 0:
                    times[key] = record["seconds"]
            failed = failed or code != 0
        if failed:
            return False
        rep += 1
        if max_pairs is not None and rep >= reps:
            if sequential_decision(pair_log_ratios(module, name, rep, times, engines))[1]:
                break
    return True


def pair_log_ratios(module, name, pairs, times, engines=ENGINES):
    """log(t_A / t_B) for each of the first `pairs` repetitions with both runs timed."""
    portal, reference = engines
    return [math.log(times[(module, name, portal, rep)] / times[(module, name, reference, rep)])
            for rep in range(pairs)
            if (module, name, portal, rep) in times and (module, name, reference, rep) in times
            and times[(module, name, reference, rep)] > 0 and times[(module, name, portal, rep)] > 0]


def last_line(text, limit=200):
    """The last non-blank line of a child's stderr, ANSI color codes removed."""
    lines = [line for line in re.sub(r"\x1b\[[0-9;]*m", "", text or "").splitlines()
             if line.strip()]
    return lines[-1].strip()[:limit] if lines else ""


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


def walsh_averages(values):
    """Every (x_i + x_j) / 2 with i <= j, sorted."""
    return sorted((values[i] + values[j]) / 2
                  for i in range(len(values)) for j in range(i, len(values)))


def hodges_lehmann(values):
    """The one-sample Hodges-Lehmann estimate: the median of the Walsh averages."""
    if not values:
        raise ValueError("Hodges-Lehmann estimate of an empty sample")
    return statistics.median(walsh_averages(values))


def signed_rank_counts(n):
    """counts[t]: how many of the 2**n sign patterns give the signed-rank statistic T+ = t."""
    counts = [1]
    for rank in range(1, n + 1):
        grown = counts + [0] * rank
        for total, count in enumerate(counts):
            grown[total + rank] += count
        counts = grown
    return counts


def wilcoxon_interval(values, alpha=CS_ALPHA):
    """The exact distribution-free interval for the pseudo-median, from the signed-rank test.

    With Walsh averages W_(1) <= ... <= W_(M), M = n(n+1)/2, the interval is
    [W_(k), W_(M+1-k)], where c = k - 1 is the largest value with P(T+ <= c) <= alpha/2
    under the null. Returns (low, high, coverage), where coverage = 1 - 2 P(T+ <= c) is
    at least 1 - alpha. Returns None when n is too small for any k (n <= 5 at alpha 0.05).
    Valid at a fixed n; after sequential stopping, `median_confidence_sequence` is the
    valid interval.
    """
    n = len(values)
    counts, total = signed_rank_counts(n), 2 ** n
    below, k = 0, 0
    for statistic, count in enumerate(counts):
        if (below + count) / total > alpha / 2:
            break
        below += count
        k = statistic + 1
    if k == 0:
        return None
    walsh = walsh_averages(values)
    return walsh[k - 1], walsh[len(walsh) - k], 1 - 2 * below / total


def sign_log_evalue(positives, n, prior=CS_PRIOR):
    """log of the beta-binomial mixture e-value of `positives` of n signs against a fair coin.

    E = B(prior + k, prior + n - k) / B(prior, prior) * 2**n is a nonnegative martingale
    with mean 1 when each sign is a fair coin given the past. By Ville's inequality it
    ever reaches 1/alpha with probability at most alpha.
    """
    def log_beta(a, b):
        return math.lgamma(a) + math.lgamma(b) - math.lgamma(a + b)
    return (log_beta(prior + positives, prior + n - positives) - log_beta(prior, prior)
            + n * math.log(2))


def median_confidence_sequence(values, alpha=CS_ALPHA, prior=CS_PRIOR):
    """The anytime-valid confidence interval for the median after every prefix of `values`.

    At n observations the set {m : E_n(m) < 1/alpha} uses E's dependence on m only through
    K(m) = #{x_i > m}. Its accepted K form one run [k_lo, k_hi], because log B is convex in
    k, so the set is the order-statistic interval [x_(n - k_hi), x_(n - k_lo + 1)], with
    x_(0) = -inf and x_(n+1) = +inf. The running intersection over n is returned as
    (low, high). It holds the true median at every n at once with probability >= 1 - alpha,
    for i.i.d. continuous values. Returns None when the intersection is empty, which has
    probability at most alpha.
    """
    low, high, threshold = -math.inf, math.inf, -math.log(alpha)
    for n in range(1, len(values) + 1):
        ordered = sorted(values[:n])
        accepted = [k for k in range(n + 1) if sign_log_evalue(k, n, prior) < threshold]
        k_lo, k_hi = accepted[0], accepted[-1]
        low = max(low, ordered[n - k_hi - 1] if n - k_hi >= 1 else -math.inf)
        high = min(high, ordered[n - k_lo] if k_lo >= 1 else math.inf)
    return None if low > high else (low, high)


def sequential_decision(log_ratios, thresholds=DECISION_THRESHOLDS,
                        tolerance=DECISION_TOLERANCE, alpha=CS_ALPHA):
    """Each threshold's verdict from the confidence sequence, and whether all are decided.

    A threshold is decided when the sequence lies wholly below it, wholly above it, or
    within +-5% of it. An empty sequence (probability <= alpha) decides nothing.
    """
    sequence = median_confidence_sequence(log_ratios, alpha) if log_ratios else None
    verdicts = {}
    for threshold in thresholds:
        point, slack = math.log(threshold), math.log(tolerance)
        if sequence is None:
            verdict = None
        elif sequence[1] < point:
            verdict = "below"
        elif sequence[0] > point:
            verdict = "above"
        elif sequence[0] >= point - slack and sequence[1] <= point + slack:
            verdict = "within 5%"
        else:
            verdict = None
        verdicts[f"{threshold:g}x"] = verdict
    return verdicts, all(verdict is not None for verdict in verdicts.values())


def scene_ratios(runs):
    """Per-scene ratio summaries and the excluded scenes, from run records.

    A repetition counts when both engines exited 0 in it. The scene ratio is
    the median of those paired portal/reference ratios. A scene with no
    complete repetition is excluded and its reason recorded: the side(s)
    that failed or timed out, or a side never run.
    """
    by_scene = {}
    for record in timing_runs(runs):
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
        failed = sorted({record["engine"] + (":timeout" if record["exit"] is None else
                                             f":exit{record['exit']}")
                         for record in cells.values() if record["exit"] != 0})
        if not pairs:
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
            # A later repetition that failed after complete pairs: the ratio
            # stands on the pairs, and the failure stays visible beside it.
            "failed": failed,
        }
    return included, excluded


def timing_runs(runs):
    """The timed runs: cachegrind instruction runs are 30x slower and never timed."""
    return [record for record in runs if record.get("kind", "timing") == "timing"]


def scene_calibration(runs):
    """Per scene: the pair rows and the calibration summary (fm-perf-calibration-lane-pkn8).

    Pair rows carry scene, pair_index, order, t_a (portal), t_b (Reference), log_ratio,
    instr_a, instr_b, load_start, load_end and qualification. A pair counts when both
    runs exited 0. The summary holds:
    - hl_ratio, exp of the Hodges-Lehmann log-ratio;
    - ci_low and ci_high, the exact Wilcoxon interval (null below 6 pairs);
    - cs_low and cs_high, the anytime-valid sequence, and the decision it gives;
    - stopped_at_pair;
    - instruction_ratio: portal/Reference retired instructions, from perf on the timed runs
      or from the cachegrind runs (medians), with its source.
    """
    cells, instructions = {}, {}
    for record in runs:
        scene = (record["module"], record["scene"])
        if record.get("kind") == "instructions":
            if record.get("exit") == 0 and record.get("instructions"):
                instructions.setdefault(scene, {}).setdefault(record["engine"], []).append(
                    record["instructions"])
            continue
        cells.setdefault(scene, {})[(record["engine"], record["rep"])] = record
    pairs, summaries = [], {}
    for scene, cell in sorted(cells.items()):
        rows = []
        for rep in sorted({rep for _, rep in cell}):
            a, b = cell.get(("portal", rep)), cell.get(("reference", rep))
            if not (a and b and a["exit"] == 0 and b["exit"] == 0 and a["seconds"] > 0
                    and b["seconds"] > 0):
                continue
            first, second = (a, b) if a.get("order", "AB") == "AB" else (b, a)
            rows.append({"module": scene[0], "scene": scene[1], "pair_index": rep,
                         "order": a.get("order", "AB"), "t_a": a["seconds"], "t_b": b["seconds"],
                         "log_ratio": math.log(a["seconds"] / b["seconds"]),
                         "instr_a": a.get("instructions"), "instr_b": b.get("instructions"),
                         "load_start": first.get("load_start"), "load_end": second.get("load_end"),
                         "qualification": QUALIFICATION})
        pairs += rows
        if not rows:
            continue
        logs = [row["log_ratio"] for row in rows]
        interval = wilcoxon_interval(logs)
        sequence = median_confidence_sequence(logs)
        verdicts, decided = sequential_decision(logs)
        summary = {"module": scene[0], "scene": scene[1], "pairs": len(rows),
                   "stopped_at_pair": max(row["pair_index"] for row in rows) + 1,
                   "hl_ratio": math.exp(hodges_lehmann(logs)),
                   "ci_low": None if interval is None else math.exp(interval[0]),
                   "ci_high": None if interval is None else math.exp(interval[1]),
                   "ci_coverage": None if interval is None else interval[2],
                   "cs_low": None if sequence is None else math.exp(sequence[0]),
                   "cs_high": None if sequence is None else math.exp(sequence[1]),
                   "decision": verdicts, "decided": decided,
                   "instruction_ratio": None, "instruction_source": "unavailable",
                   "qualification": QUALIFICATION}
        perf = [(row["instr_a"], row["instr_b"]) for row in rows if row["instr_a"] and row["instr_b"]]
        counted = instructions.get(scene, {})
        if perf:
            summary["instruction_ratio"] = statistics.median(a / b for a, b in perf)
            summary["instruction_source"] = "perf stat on the timed runs"
        elif counted.get("portal") and counted.get("reference"):
            summary["instruction_ratio"] = (statistics.median(counted["portal"])
                                            / statistics.median(counted["reference"]))
            summary["instruction_source"] = "cachegrind, warm runs apart from the timed pairs"
        summaries[scene] = summary
    return pairs, summaries


def timeout_bounds(runs, excluded):
    """Lower bounds on the ratio of scenes excluded only by portal timeouts.

    A repetition whose portal timed out while its Reference succeeded has a
    ratio of at least portal elapsed / Reference seconds. A scene's bound is
    the median of those per-repetition bounds (componentwise lower bounds
    bound the median). Scenes that also failed another way have none.
    """
    cells = {}
    for record in runs:
        cells.setdefault((record["module"], record["scene"]), {})[
            (record["engine"], record["rep"])] = record
    bounds = {}
    for scene, reasons in excluded.items():
        if reasons != ["portal:timeout"]:
            continue
        per_rep = [portal["seconds"] / reference["seconds"]
                   for (engine, rep), portal in cells[scene].items()
                   if engine == "portal" and portal["exit"] is None
                   for reference in [cells[scene].get(("reference", rep))]
                   if reference and reference["exit"] == 0 and reference["seconds"] > 0]
        if per_rep:
            bounds[scene] = statistics.median(per_rep)
    return bounds


def censored_percentile(ratios, bounds, q):
    """percentile(ratios + bounds, q) when no lower bound can move it, else None.

    Each bound's true ratio is at least the bound. The interpolated percentile
    reads the sorted values at the index below its position and, unless the
    position is whole, the one above; it is exact when every bound sorts
    strictly after those (ties sort measured values first).
    """
    ordered = sorted([(value, False) for value in ratios] + [(value, True) for value in bounds])
    if not ordered:
        return None
    position = (len(ordered) - 1) * q / 100.0
    low = int(position)
    read = low if position == low else min(low + 1, len(ordered) - 1)
    if any(is_bound for _, is_bound in ordered[:read + 1]):
        return None
    return percentile([value for value, _ in ordered], q)


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

def write_dashboard(out_dir: pathlib.Path, dashboard: pathlib.Path, measured_commit=None):
    logged = load_runs(out_dir / "runs.ndjson")
    runs = timing_runs(logged)
    pairs, calibration = scene_calibration(logged)
    (out_dir / "pairs.ndjson").write_text(
        "".join(json.dumps(row, sort_keys=True) + "\n" for row in pairs), encoding="utf-8")
    (out_dir / "calibration.ndjson").write_text(
        "".join(json.dumps(row, sort_keys=True) + "\n" for row in calibration.values()),
        encoding="utf-8")
    header = json.loads((out_dir / "header.json").read_text(encoding="utf-8")) \
        if (out_dir / "header.json").exists() else {}
    included, excluded = scene_ratios(runs)
    summary = corpus_summary(included)
    skip_note = (
        " Both engines step skipped segments on the frame grid without capturing: the Reference"
        " runs under a two-method in-process replacement (--reference-frame-grid), so both do the"
        " same updater work (BN-10)."
        if header.get("mode") == FRAME_GRID_MODE else
        " Skip mode differs by design (BN-10): the portal runs every frame's updaters, the"
        " Reference one step per skipped segment.")
    lines = [
        "# Corpus speed ratio: portal vs pinned Reference",
        "",
        f"**{LABEL}.** Generated by `scripts/corpus_speed_ratio.py` (fm-5wq.35, schema"
        f" `{SCHEMA}` v{VERSION}). Ratio = portal wall time / Reference wall time, paired per"
        " interleaved repetition, median per scene. Below 1 means the portal is faster. Not PG-1"
        " evidence." + skip_note,
        "",
        "| host fact | value |",
        "|---|---|",
    ]
    for key in ("cpu", "logical_cpus", "governor", "platform", "portal_id", "reference_id",
                "reps", "timeout_s", "mode", "order", "max_pairs", "instructions", "selection"):
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
    bounds = timeout_bounds(runs, excluded)
    if bounds:
        # Dropping portal timeouts flatters the portal: they are its slowest
        # scenes. Count them at their lower bounds wherever that is exact.
        ratios = [row["ratio"] for row in included.values()]
        for name, q in (("median", 50), ("p90", 90), ("p99", 99)):
            value = censored_percentile(ratios, list(bounds.values()), q)
            shown = "not determinable (a timeout bound reaches it)" if value is None else f"{value:.2f}"
            lines.append(f"| ratio {name} counting {len(bounds)} portal timeouts at their lower"
                         f" bounds | {shown} |")
    if calibration:
        def shown(value, digits=3):
            return "n/a" if value is None else f"{value:.{digits}f}"
        lines += ["", f"## Paired calibration ({QUALIFICATION}; ABBA pairs, log-ratios)", "",
                  "Hodges-Lehmann ratio with the exact Wilcoxon interval (valid at a fixed pair"
                  " count), and the anytime-valid confidence sequence for the median ratio"
                  " (valid under the sequential stopping it drives). Verdicts are per threshold:"
                  " below, above, or within 5%.", "",
                  "| scene | pairs | HL ratio | Wilcoxon 95% | sequence 95% | 0.5x | 1x |"
                  " instruction ratio |",
                  "|---|---:|---:|---|---|---|---|---|"]
        for (module, name), row in sorted(calibration.items(), key=lambda item: -item[1]["hl_ratio"]):
            verdict = row["decision"]
            lines.append(
                f"| {module}:{name} | {row['pairs']} | {row['hl_ratio']:.3f} |"
                f" {shown(row['ci_low'])}-{shown(row['ci_high'])} |"
                f" {shown(row['cs_low'])}-{shown(row['cs_high'])} |"
                f" {verdict['0.5x'] or 'undecided'} | {verdict['1x'] or 'undecided'} |"
                f" {shown(row['instruction_ratio'])} ({row['instruction_source']}) |")
    lines += ["", "## Per scene (slowest first)", "",
              "| scene | ratio | min | max | pairs | portal s | Reference s | later failures |",
              "|---|---:|---:|---:|---:|---:|---:|---|"]
    for (module, name), row in sorted(included.items(), key=lambda item: -item[1]["ratio"]):
        lines.append(f"| {module}:{name} | {row['ratio']:.2f} | {row['min']:.2f} |"
                     f" {row['max']:.2f} | {row['pairs']} | {row['portal_s']:.1f} |"
                     f" {row['reference_s']:.1f} | {', '.join(row['failed'])} |")
    if excluded:
        errors, sides = {}, {}
        for record in runs:
            scene = (record["module"], record["scene"])
            if record["exit"] != 0 and record.get("error"):
                errors.setdefault(scene, record["error"])
            outcome = ("ok" if record["exit"] == 0 else
                       "timeout" if record["exit"] is None else f"exit {record['exit']}")
            sides.setdefault(scene, {}).setdefault(
                record["engine"], f"{outcome} {record['seconds']:.1f} s")
        lines += ["", "## Excluded (never imputed)", "",
                  "| scene | failed side | portal | Reference | last stderr line |",
                  "|---|---|---|---|---|"]
        for scene, reasons in excluded.items():
            error = errors.get(scene, "").replace("|", "\\|")
            portal = sides.get(scene, {}).get("portal", "not run")
            reference = sides.get(scene, {}).get("reference", "not run")
            lines.append(f"| {scene[0]}:{scene[1]} | {', '.join(reasons)} | {portal} |"
                         f" {reference} | {error} |")
    dashboard.parent.mkdir(parents=True, exist_ok=True)
    if measured_commit:
        from check_measurement_staleness import producer_header
        ids = sorted({f"{r['module']}:{r['scene']}" for r in runs})
        lines.insert(0, producer_header(measured_commit, ids))
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


def parse_perf_instructions(text):
    """The `instructions` count from `perf stat -x ,` output, or None if it was not counted."""
    for line in (text or "").splitlines():
        fields = line.split(",")
        if len(fields) >= 3 and fields[2].startswith("instructions"):
            return int(fields[0]) if fields[0].isdigit() else None
    return None


def perf_unavailable():
    """None when `perf stat` counts instructions on this host, else why it cannot.

    An unprivileged user on a host with perf_event_paranoid above 2 gets no events.
    The run is then recorded with instructions unavailable, never silently uncounted.
    """
    try:
        result = subprocess.run(["perf", "stat", "-x", ",", "-e", "instructions", "--", "true"],
                                capture_output=True, text=True, timeout=60, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        return f"perf unavailable: {error}"
    if result.returncode != 0 or parse_perf_instructions(result.stderr) is None:
        try:
            paranoid = pathlib.Path("/proc/sys/kernel/perf_event_paranoid").read_text().strip()
            return (f"perf unavailable: perf_event_paranoid={paranoid} (an unprivileged user"
                    " needs <= 2)")
        except OSError:
            return "perf unavailable: " + (last_line(result.stderr) or f"exit {result.returncode}")
    return None


def sum_cachegrind(out_dir: pathlib.Path):
    """Total `summary:` instructions over one run's cachegrind files (children included)."""
    total, seen = 0, False
    for path in sorted(out_dir.glob("cg.*")):
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            if line.startswith("summary:"):
                total += int(line.split()[1])
                seen = True
    return total if seen else None


# A cachegrind count is load-independent only single-threaded. Valgrind serializes threads,
# and a waiting worker that spins retires instructions in proportion to its time slices: Hello's
# counts fell 10% (portal) and 20% (Reference, llvmpipe) at load 52. Single-threaded (portal
# --threads 1, these variables, fixed hash seed) they moved 0.0002% and 0.0035% at load 16.
SINGLE_THREADED = {"PYTHONHASHSEED": "0", "OMP_NUM_THREADS": "1", "OPENBLAS_NUM_THREADS": "1",
                   "MKL_NUM_THREADS": "1", "LP_NUM_THREADS": "1"}


def instrument(argv, env, tool, stem: pathlib.Path):
    """(argv, env, read) that count one run's retired instructions with `tool`.

    `read()` returns the count after the run, or None. `perf` wraps the measured
    process itself, so its timed run stays timed (and counts every thread as
    configured). `cachegrind` runs under valgrind, about 30x slower and never timed,
    single-threaded (SINGLE_THREADED; the portal runner adds --threads 1).
    """
    if tool == "perf":
        out = stem.parent / (stem.name + ".perf.csv")

        def read_perf():
            try:
                return parse_perf_instructions(out.read_text(encoding="utf-8"))
            except OSError:
                return None
        return (["perf", "stat", "-x", ",", "-e", "instructions", "-o", str(out), "--", *argv],
                env, read_perf)
    if tool == "cachegrind":
        out_dir = stem.parent / f"{stem.name}.cachegrind-{time.time_ns()}"
        out_dir.mkdir(parents=True, exist_ok=True)
        return (["valgrind", "--tool=cachegrind", "--cache-sim=no", "--trace-children=yes",
                 f"--cachegrind-out-file={out_dir}/cg.%p", *argv],
                dict(env, **SINGLE_THREADED), lambda: sum_cachegrind(out_dir))
    return argv, env, lambda: None


def measure_instructions(scene, runners, log, done, lock, engines=ENGINES):
    """One cachegrind run per engine after a scene's timed pairs (warm caches)."""
    module, name = scene
    for engine in engines:
        key = (module, name, engine, "instructions")
        if key in done:
            continue
        start = time.monotonic()
        code, stderr, extra = runners[engine](scene, "instructions", "cachegrind")
        record = {"schema": SCHEMA, "version": VERSION, "label": LABEL,
                  "qualification": QUALIFICATION, "kind": "instructions", "module": module,
                  "scene": name, "engine": engine, "rep": "instructions", "exit": code,
                  "seconds": round(time.monotonic() - start, 4), **extra}
        if code != 0:
            record["error"] = last_line(stderr)
        with lock:
            log(record)
            done[key] = code


def make_runners(args, display, timed_tool=None):
    """Runner per engine: (scene, rep[, tool]) -> (exit code, stderr, extra fields).

    `timed_tool` instruments every timed run (perf); a tool passed per call
    instruments that run only (cachegrind).
    """
    work = args.out / "work"

    def counted(argv, env, stem, tool):
        argv, env, read = instrument(argv, env, tool, stem)
        code, stderr = run_child(argv, args.workdir, env, args.timeout)[:2]
        extra = {} if tool is None else {"instructions": read(), "instructions_tool": tool}
        return code, stderr, extra

    def portal(scene, rep, tool=None):
        module, name = scene
        target = work / f"{module.replace('/', '__')}__{name}" / f"portal-{rep}.png"
        target.parent.mkdir(parents=True, exist_ok=True)
        env = dict(os.environ, PYTHONPATH=str(args.videos))
        if args.census:
            # The portal's segment census (fm-offload-payoff-4rve), one file per run.
            env["FMN_SEGMENT_CENSUS"] = str(target.with_name(f"census-{rep}.ndjson"))
        if args.frames:
            argv = [args.portal_python, "-m", "fmn_python", str(args.videos / module), name,
                    "--format", "mp4", "--resolution", "320x180", "--fps", FRAMES_FPS,
                    "--threads", "1", "--video_dir", str(target.with_name(f"portal-{rep}"))]
        else:
            argv = [args.portal_python, "-m", "fmn_python", str(args.videos / module), name, "-s",
                    "--format", "png", "--resolution", "320x180", "--video_dir", str(target)]
        if tool == "cachegrind" and "--threads" not in argv:
            argv += ["--threads", "1"]
        return counted(argv, env, target.with_name(f"portal-{rep}"), tool or timed_tool)

    def reference(scene, rep, tool=None):
        module, name = scene
        target = work / f"{module.replace('/', '__')}__{name}" / f"reference-{rep}"
        target.mkdir(parents=True, exist_ok=True)
        env = {"PATH": "/usr/bin:/bin", "HOME": os.environ.get("HOME", "/tmp"),
               "DISPLAY": display,
               "PYTHONPATH": f"{args.reference_root}{os.pathsep}{args.videos}"}
        argv = reference_argv(args, module, name, target)
        return counted(argv, env, target, tool or timed_tool)

    return {"portal": portal, "reference": reference}


def reference_argv(args, module, name, target):
    entry = ["-c", REFERENCE_FRAME_GRID] if args.reference_frame_grid else ["-m", "manimlib"]
    if args.frames:
        return [args.reference_python, *entry, str(args.videos / module), name,
                "-w", "-r", "320x180", "--fps", FRAMES_FPS, "--video_dir", str(target)]
    return [args.reference_python, *entry, str(args.videos / module), name, "-s",
            "-w", "-r", "320x180", "--video_dir", str(target)]


def select_scenes(args):
    """The scenes to measure and a description of how they were chosen."""
    if args.scene:
        return [tuple(item.split(":", 1)) for item in args.scene], "explicit --scene list"
    scenes = []
    for line in args.candidates.read_text(encoding="utf-8").splitlines():
        if line.strip():
            record = json.loads(line)
            if record.get("outcome", "ok") == "ok":
                scenes.append((record["module"], record["scene"]))
    scenes = sorted(set(scenes))
    source = f"{len(scenes)} candidates in {args.candidates.name}"
    if args.sample and args.sample < len(scenes):
        chosen = random.Random(args.sample_seed).sample(scenes, args.sample)
        return sorted(chosen), f"seeded sample of {args.sample} (seed {args.sample_seed}) of {source}"
    return scenes, f"all {source}"


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
    parser.add_argument("--frames", action="store_true",
                        help="render every frame in both engines instead of the final state")
    parser.add_argument("--census", action="store_true",
                        help="record the portal's segment census beside each portal run")
    parser.add_argument("--reference-frame-grid", action="store_true",
                        help="step the Reference's skipped segments on the frame grid, as the"
                             " portal does (BN-10), so both engines do the same work")
    parser.add_argument("--max-pairs", type=int, default=None,
                        help="run each scene until the confidence sequence decides 0.5x and 1.0x,"
                             " at least --reps and at most this many ABBA pairs")
    parser.add_argument("--instructions", choices=("none", "perf", "cachegrind"), default="none",
                        help="count retired instructions: perf on every timed run (where the host"
                             " allows), or one warm cachegrind run per engine after the pairs")
    parser.add_argument("--report", type=pathlib.Path, help="derive the dashboard from DIR")
    parser.add_argument("--dashboard", type=pathlib.Path)
    parser.add_argument("--measured-commit",
                        help="the commit the measured portal wheel was built from; stamps the"
                             " dashboard's staleness header (check_measurement_staleness.py)")
    args = parser.parse_args()

    if args.report:
        dashboard = args.dashboard or args.report / "speed_ratio.md"
        summary, excluded = write_dashboard(args.report, dashboard, args.measured_commit)
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
    scenes, selection = select_scenes(args)
    timed_tool, instructions = None, args.instructions
    if args.instructions == "perf":
        reason = perf_unavailable()
        timed_tool, instructions = (None, reason) if reason else ("perf", "perf")
    header = dict(host_facts(), portal_id=args.portal_id, reference_id=args.reference_id,
                  reps=args.reps, timeout_s=args.timeout,
                  mode=(FRAMES_MODE if args.frames else
                        FRAME_GRID_MODE if args.reference_frame_grid else SKIP_MODE),
                  order=ORDER, max_pairs=args.max_pairs, instructions=instructions,
                  qualification=QUALIFICATION, selection=selection, label=LABEL, schema=SCHEMA,
                  version=VERSION)
    header_path, log_path = args.out / "header.json", args.out / "runs.ndjson"
    if log_path.exists() and header_path.exists():
        conflicts = resume_conflicts(json.loads(header_path.read_text(encoding="utf-8")), header)
        if conflicts:
            parser.error(f"{args.out} was started with different {', '.join(conflicts)};"
                         " resume with the same settings or use a new --out")
    header_path.write_text(json.dumps(header, indent=1) + "\n", encoding="utf-8")
    logged = load_runs(log_path)
    done = {run_key(record): record["exit"] for record in logged}
    times = {run_key(record): record["seconds"] for record in timing_runs(logged)
             if record["exit"] == 0}
    lock = threading.Lock()
    handle = log_path.open("a", encoding="utf-8")

    def log(record):
        handle.write(json.dumps(record, sort_keys=True) + "\n")
        handle.flush()

    def measure(scene):
        finished = measure_scene(scene, args.reps, runners, log, done, lock,
                                 max_pairs=args.max_pairs, times=times)
        if finished and args.instructions == "cachegrind":
            measure_instructions(scene, runners, log, done, lock)

    with XvfbServer() as xvfb:
        runners = make_runners(args, xvfb.display, timed_tool)
        with ThreadPoolExecutor(max_workers=max(1, args.jobs)) as pool:
            list(pool.map(measure, scenes))
    handle.close()
    print(json.dumps({"schema": SCHEMA, "scenes": len(scenes), "runs": str(log_path)}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
