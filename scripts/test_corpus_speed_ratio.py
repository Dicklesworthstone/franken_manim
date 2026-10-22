#!/usr/bin/env python3
"""Unit tests for scripts/corpus_speed_ratio.py (fm-5wq.35).

The producer's statistics and ordering are deterministic and tested on
constructed inputs. The planted negative injects a fixed sleep on the portal
side of one scene, through in-process runners, and checks that scene's ratio
moves by the expected amount.
"""
import pathlib
import sys
import tempfile
import threading
import time
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import corpus_speed_ratio as csr  # noqa: E402


def run(module, scene, engine, rep, seconds, exit_code=0):
    return {"module": module, "scene": scene, "engine": engine, "rep": rep,
            "seconds": seconds, "exit": exit_code}


class Collect:
    def __init__(self):
        self.records, self.done, self.lock = [], {}, threading.Lock()

    def __call__(self, record):
        self.records.append(record)


class SpeedRatioTests(unittest.TestCase):
    def test_bootstrap_is_seeded_and_deterministic(self):
        values = [0.5, 0.8, 1.2, 2.0, 3.5, 0.9, 1.1]
        median = lambda sample: csr.percentile(sample, 50)  # noqa: E731
        first = csr.bootstrap_interval(values, median, resamples=500, seed=7)
        self.assertEqual(first, csr.bootstrap_interval(values, median, resamples=500, seed=7))
        low, high = first
        self.assertLessEqual(low, median(values))
        self.assertGreaterEqual(high, median(values))
        # The seed matters. Over 7 values the resample statistics are few and
        # the interval's order statistics can coincide across seeds; over 50
        # distinct values they do not.
        spread = [1.0 + 0.037 * index * index % 5.3 for index in range(50)]
        mean = lambda sample: sum(sample) / len(sample)  # noqa: E731
        self.assertNotEqual(csr.bootstrap_interval(spread, mean, resamples=500, seed=7),
                            csr.bootstrap_interval(spread, mean, resamples=500, seed=8))

    def test_percentile_is_linear_interpolation(self):
        self.assertEqual(csr.percentile([1, 2, 3, 4], 50), 2.5)
        self.assertEqual(csr.percentile([10], 99), 10)
        self.assertAlmostEqual(csr.percentile([0, 10], 90), 9.0)
        with self.assertRaises(ValueError):
            csr.percentile([], 50)

    def test_ratio_is_the_median_of_paired_repetitions(self):
        runs = [run("m.py", "A", "portal", 0, 2.0), run("m.py", "A", "reference", 0, 1.0),
                run("m.py", "A", "portal", 1, 3.0), run("m.py", "A", "reference", 1, 1.0),
                run("m.py", "A", "portal", 2, 1.0), run("m.py", "A", "reference", 2, 2.0)]
        included, excluded = csr.scene_ratios(runs)
        row = included[("m.py", "A")]
        self.assertEqual(row["ratio"], 2.0)  # median of 2.0, 3.0, 0.5
        self.assertEqual((row["min"], row["max"], row["pairs"]), (0.5, 3.0, 3))
        self.assertEqual(excluded, {})

    def test_a_missing_or_failed_side_is_excluded_and_counted(self):
        runs = [run("m.py", "Timeout", "portal", 0, 300.0, None),
                run("m.py", "Refail", "portal", 0, 1.0), run("m.py", "Refail", "reference", 0, 1.0, 1),
                run("m.py", "Half", "portal", 0, 1.0),
                run("m.py", "Ok", "portal", 0, 1.0), run("m.py", "Ok", "reference", 0, 4.0)]
        included, excluded = csr.scene_ratios(runs)
        self.assertEqual(list(included), [("m.py", "Ok")])
        self.assertEqual(included[("m.py", "Ok")]["ratio"], 0.25)
        self.assertEqual(excluded[("m.py", "Timeout")], ["portal:timeout"])
        self.assertEqual(excluded[("m.py", "Refail")], ["reference:exit1"])
        self.assertEqual(excluded[("m.py", "Half")], ["incomplete"])
        self.assertEqual(csr.corpus_summary(included)["scenes"], 1)
        self.assertEqual(included[("m.py", "Ok")]["failed"], [])

    def test_portal_timeouts_count_at_their_lower_bounds_only_where_exact(self):
        runs = [run("m.py", s, e, 0, sec)
                for s, factor in (("A", 0.5), ("B", 1.0), ("C", 2.0))
                for e, sec in (("portal", factor), ("reference", 1.0))]
        runs += [run("m.py", "T1", "portal", 0, 300.0, None), run("m.py", "T1", "reference", 0, 30.0),
                 run("m.py", "T2", "portal", 0, 300.0, None), run("m.py", "T2", "reference", 0, 15.0),
                 run("m.py", "Both", "portal", 0, 300.0, None), run("m.py", "Both", "reference", 0, 2.0, 1),
                 run("m.py", "Refail", "portal", 0, 1.0), run("m.py", "Refail", "reference", 0, 1.0, 1)]
        included, excluded = csr.scene_ratios(runs)
        bounds = csr.timeout_bounds(runs, excluded)
        self.assertEqual(bounds, {("m.py", "T1"): 10.0, ("m.py", "T2"): 20.0})
        ratios = [row["ratio"] for row in included.values()]
        # Dropping the timeouts would report 1.0; counted at their bounds the
        # median is the third of five values, which no bound can move.
        self.assertEqual(csr.percentile(ratios, 50), 1.0)
        self.assertEqual(csr.censored_percentile(ratios, list(bounds.values()), 50), 2.0)
        # p90 reads the two largest values, both only bounded: unknown.
        self.assertIsNone(csr.censored_percentile(ratios, list(bounds.values()), 90))
        # A bound inside the interpolated pair makes the median unknown too.
        self.assertIsNone(csr.censored_percentile([0.5, 1.0, 2.0], [0.7], 50))
        with tempfile.TemporaryDirectory() as tmp:
            out = pathlib.Path(tmp)
            (out / "runs.ndjson").write_text("\n".join(
                __import__("json").dumps(r) for r in runs) + "\n")
            csr.write_dashboard(out, out / "dash.md")
            text = (out / "dash.md").read_text()
        self.assertIn("| ratio median counting 2 portal timeouts at their lower bounds | 2.00 |", text)
        self.assertIn("| ratio p90 counting 2 portal timeouts at their lower bounds |"
                      " not determinable (a timeout bound reaches it) |", text)

    def test_frame_grid_mode_runs_the_reference_through_the_replacement_program(self):
        args = type("Args", (), {"reference_python": "py", "videos": pathlib.Path("/v"),
                                 "reference_frame_grid": False, "frames": False})()
        plain = csr.reference_argv(args, "m.py", "A", pathlib.Path("/t"))
        self.assertEqual(plain[:3], ["py", "-m", "manimlib"])
        # --frames renders every frame: no -s, the frame rate pinned.
        args.frames = True
        frames = csr.reference_argv(args, "m.py", "A", pathlib.Path("/t"))
        self.assertNotIn("-s", frames)
        self.assertEqual(frames[frames.index("--fps") + 1], csr.FRAMES_FPS)
        self.assertIn("-s", plain)
        args.frames = False
        args.reference_frame_grid = True
        grid = csr.reference_argv(args, "m.py", "A", pathlib.Path("/t"))
        self.assertEqual(grid[:3], ["py", "-c", csr.REFERENCE_FRAME_GRID])
        # Everything after the entry point is the same Reference command line.
        self.assertEqual(grid[3:], plain[3:])
        self.assertNotEqual(csr.FRAME_GRID_MODE, csr.SKIP_MODE)
        self.assertIn("mode", csr.IDENTITY)
        runs = [run("m.py", "A", "portal", 0, 2.0), run("m.py", "A", "reference", 0, 1.0)]
        for mode, note in ((csr.FRAME_GRID_MODE, "same updater work"),
                           (csr.SKIP_MODE, "one step per skipped segment")):
            with tempfile.TemporaryDirectory() as tmp:
                out = pathlib.Path(tmp)
                (out / "header.json").write_text(__import__("json").dumps({"mode": mode}))
                (out / "runs.ndjson").write_text("\n".join(
                    __import__("json").dumps(r) for r in runs) + "\n")
                csr.write_dashboard(out, out / "dash.md")
                text = (out / "dash.md").read_text()
            self.assertIn(note, text)
            self.assertIn(f"| mode | {mode} |", text)

    def test_frame_grid_program_steps_skipped_segments_like_playback(self):
        # Run the program exactly as the producer does (python -c), over
        # stand-in Reference modules with its skip behavior (scene.py:467,
        # :567). The stand-in entry point reports what the program installed.
        import json
        import subprocess
        stand_ins = {
            "manimlib/__init__.py": "",
            "manimlib/scene/__init__.py": "",
            "manimlib/scene/scene.py": (
                "class Scene:\n"
                "    skip_animations, fps = True, 4\n"
                "    def get_time_progression(self, run_time, n_iterations=None, desc='',\n"
                "                             override_skip_animations=False):\n"
                "        if self.skip_animations and not override_skip_animations:\n"
                "            return [run_time]\n"
                "        return [(i + 1) / self.fps for i in range(int(run_time * self.fps))]\n"
                "    def finish_animations(self, animations):\n"
                "        for animation in animations:\n"
                "            animation.finish()\n"
                "        self.update_mobjects(self.get_time_progression(1.0)[-1])\n"
                "    def update_mobjects(self, dt):\n"
                "        self.calls.append(['update_mobjects', dt])\n"),
            "manimlib/__main__.py": (
                "import json, sys\n"
                "from manimlib.scene.scene import Scene\n"
                "class Animation:\n"
                "    def __init__(self, calls): self.calls = calls\n"
                "    def finish(self): self.calls.append('finish')\n"
                "    def clean_up_from_scene(self, scene): self.calls.append('clean_up')\n"
                "def main():\n"
                "    scene = Scene()\n"
                "    scene.calls = []\n"
                "    grid = scene.get_time_progression(1.0)\n"
                "    scene.finish_animations([Animation(scene.calls)])\n"
                "    print(json.dumps({'argv': sys.argv, 'grid': grid, 'finish': scene.calls}))\n"),
        }
        with tempfile.TemporaryDirectory() as tmp:
            for relative, text in stand_ins.items():
                path = pathlib.Path(tmp) / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
            proc = subprocess.run([sys.executable, "-c", csr.REFERENCE_FRAME_GRID, "/v/m.py", "A", "-s"],
                                  env={"PYTHONPATH": tmp, "PATH": "/usr/bin:/bin"},
                                  capture_output=True, text=True, timeout=60)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        try:
            report = json.loads(proc.stdout)
        except json.JSONDecodeError:
            self.fail(f"the stand-in entry point printed no report: {proc.stdout!r} {proc.stderr!r}")
        self.assertEqual(report["argv"], ["manimlib", "/v/m.py", "A", "-s"])
        self.assertEqual(report["grid"], [0.25, 0.5, 0.75, 1.0])
        self.assertEqual(report["finish"], ["finish", "clean_up", ["update_mobjects", 0]])

    def test_a_later_failure_keeps_the_pairs_and_stays_visible(self):
        runs = [run("m.py", "A", "portal", 0, 2.0), run("m.py", "A", "reference", 0, 1.0),
                run("m.py", "A", "portal", 1, 300.0, None)]
        included, excluded = csr.scene_ratios(runs)
        self.assertEqual(excluded, {})
        self.assertEqual((included[("m.py", "A")]["ratio"], included[("m.py", "A")]["pairs"]),
                         (2.0, 1))
        self.assertEqual(included[("m.py", "A")]["failed"], ["portal:timeout"])
        with tempfile.TemporaryDirectory() as tmp:
            out = pathlib.Path(tmp)
            (out / "runs.ndjson").write_text("\n".join(
                __import__("json").dumps(r) for r in runs) + "\n")
            csr.write_dashboard(out, out / "dash.md")
            self.assertIn("| 1 | 2.0 | 1.0 | portal:timeout |", (out / "dash.md").read_text())

    def test_runs_alternate_engine_order_abba_in_the_log(self):
        self.assertEqual(csr.interleaved_plan([("m.py", "A")], 2),
                         [(("m.py", "A"), 0, "portal"), (("m.py", "A"), 0, "reference"),
                          (("m.py", "A"), 1, "reference"), (("m.py", "A"), 1, "portal")])
        collect = Collect()
        runners = {engine: (lambda scene, rep: (0, "")) for engine in csr.ENGINES}
        csr.measure_scene(("m.py", "A"), 3, runners, collect, collect.done, collect.lock)
        self.assertEqual([(r["engine"], r["rep"], r["order"]) for r in collect.records],
                         [("portal", 0, "AB"), ("reference", 0, "AB"), ("reference", 1, "BA"),
                          ("portal", 1, "BA"), ("portal", 2, "AB"), ("reference", 2, "AB")])
        self.assertTrue(all(r["label"] == csr.LABEL for r in collect.records))
        self.assertTrue(all(r["qualification"] == "calibration-unqualified"
                            for r in collect.records))

    def test_resume_skips_finished_runs_and_failure_stops_the_scene(self):
        collect = Collect()
        collect.done[("m.py", "A", "portal", 0)] = 0
        calls = []

        def runner(engine, code):
            def call(scene, rep):
                calls.append((engine, rep))
                return code, "Traceback ...\n\x1b[31mTypeError: bad x_min\x1b[0m\n\n"
            return call

        runners = {"portal": runner("portal", 0), "reference": runner("reference", 2)}
        finished = csr.measure_scene(("m.py", "A"), 3, runners, collect, collect.done, collect.lock)
        self.assertFalse(finished)
        # portal rep 0 was already done; the reference failure stops later reps.
        self.assertEqual(calls, [("reference", 0)])
        # The failure keeps its cause; a success records none.
        self.assertEqual(collect.records[0]["error"], "TypeError: bad x_min")

    def test_a_truncated_last_line_is_dropped_but_other_damage_raises(self):
        json = __import__("json")
        whole = json.dumps(run("m.py", "A", "portal", 0, 1.0))
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "runs.ndjson"
            path.write_text(whole + "\n" + whole[:17], encoding="utf-8")
            self.assertEqual(len(csr.load_runs(path)), 1)
            path.write_text(whole[:17] + "\n" + whole + "\n", encoding="utf-8")
            with self.assertRaises(ValueError):
                csr.load_runs(path)

    def test_resume_refuses_a_different_wheel_or_settings(self):
        header = {"portal_id": "a", "reference_id": "r", "reps": 3, "timeout_s": 300,
                  "mode": "m", "schema": csr.SCHEMA, "version": csr.VERSION, "cpu": "x"}
        self.assertEqual(csr.resume_conflicts(header, dict(header, cpu="y")), [])
        self.assertEqual(csr.resume_conflicts(header, dict(header, portal_id="b", reps=5)),
                         ["portal_id", "reps"])

    def test_the_selection_is_described_as_it_was_made(self):
        json = __import__("json")
        with tempfile.TemporaryDirectory() as tmp:
            candidates = pathlib.Path(tmp) / "ok.ndjson"
            candidates.write_text("".join(
                json.dumps({"module": "m.py", "scene": f"S{i}", "outcome": "ok"}) + "\n"
                for i in range(10)) + json.dumps({"module": "m.py", "scene": "Bad",
                                                   "outcome": "error"}) + "\n")
            args = type("Args", (), {"scene": [], "candidates": candidates,
                                     "sample": 4, "sample_seed": 35})()
            sampled, described = csr.select_scenes(args)
            self.assertEqual(len(sampled), 4)
            self.assertEqual(described, "seeded sample of 4 (seed 35) of 10 candidates in ok.ndjson")
            self.assertEqual(sampled, csr.select_scenes(args)[0])
            args.sample = 0
            everything, described = csr.select_scenes(args)
            self.assertEqual((len(everything), described), (10, "all 10 candidates in ok.ndjson"))
            args.scene = ["m.py:S1"]
            self.assertEqual(csr.select_scenes(args), ([("m.py", "S1")], "explicit --scene list"))

    def test_a_failure_still_measures_the_other_side_of_its_repetition(self):
        # A portal timeout next to the Reference's time is the worst case's
        # lower bound; without the pair it is only "excluded".
        collect, calls = Collect(), []

        def runner(engine, code):
            def call(scene, rep):
                calls.append((engine, rep))
                return code, "boom" if code else ""
            return call

        runners = {"portal": runner("portal", None), "reference": runner("reference", 0)}
        self.assertFalse(csr.measure_scene(("m.py", "A"), 3, runners, collect, collect.done,
                                           collect.lock))
        self.assertEqual(calls, [("portal", 0), ("reference", 0)])

    def test_a_logged_failure_stops_the_scene_on_resume(self):
        collect = Collect()
        collect.done[("m.py", "A", "portal", 0)] = 0
        collect.done[("m.py", "A", "reference", 0)] = 1
        calls = []
        runners = {engine: (lambda scene, rep, engine=engine: calls.append((engine, rep)) or (0, ""))
                   for engine in csr.ENGINES}
        self.assertFalse(csr.measure_scene(("m.py", "A"), 3, runners, collect, collect.done,
                                           collect.lock))
        self.assertEqual(calls, [], "no repetition after the logged reference failure")

    def test_planted_portal_sleep_moves_the_ratio_by_the_expected_amount(self):
        base, planted = 0.05, 0.10

        def sleeper(extra):
            def call(scene, rep):
                time.sleep(base + (extra if scene[1] == "Planted" else 0.0))
                return 0, ""
            return call

        collect = Collect()
        runners = {"portal": sleeper(planted), "reference": sleeper(0.0)}
        for scene in (("m.py", "Control"), ("m.py", "Planted")):
            csr.measure_scene(scene, 3, runners, collect, collect.done, collect.lock)
        included, _ = csr.scene_ratios(collect.records)
        control = included[("m.py", "Control")]["ratio"]
        planted_ratio = included[("m.py", "Planted")]["ratio"]
        expected = (base + planted) / base  # 3.0
        self.assertAlmostEqual(control, 1.0, delta=0.35)
        self.assertAlmostEqual(planted_ratio, expected, delta=0.6)
        self.assertGreater(planted_ratio - control, 1.4)

    def test_dashboard_states_label_median_and_exclusions(self):
        runs = [run("m.py", s, e, r, sec)
                for s, factor in (("A", 0.5), ("B", 2.0), ("C", 1.0))
                for r in range(2) for e, sec in (("portal", factor), ("reference", 1.0))]
        runs.append(dict(run("m.py", "D", "portal", 0, 9.0, None), error="stuck | in a loop"))
        with tempfile.TemporaryDirectory() as tmp:
            out = pathlib.Path(tmp)
            (out / "runs.ndjson").write_text("\n".join(
                __import__("json").dumps(dict(r, schema=csr.SCHEMA)) for r in runs) + "\n")
            summary, excluded = csr.write_dashboard(out, out / "dash.md")
            text = (out / "dash.md").read_text()
        self.assertEqual((summary["scenes"], excluded, summary["median"]), (3, 1, 1.0))
        self.assertIn(csr.LABEL, text)
        self.assertIn("| m.py:B | 2.00 |", text)
        self.assertIn("| m.py:D | portal:timeout | timeout 9.0 s | not run | stuck \\| in a loop |", text)


class FakeClock:
    """A monotonic clock that runners advance by exact amounts."""

    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now


class CalibrationStatisticsTests(unittest.TestCase):
    # Two-sided 0.05 critical values of the Wilcoxon signed-rank statistic: the largest c
    # with P(T+ <= c) <= 0.025 (standard tables, e.g. Hollander & Wolfe, Table A.4).
    CRITICAL_005 = {6: 0, 7: 2, 8: 3, 9: 5, 10: 8, 11: 10, 12: 13, 13: 17, 14: 21, 15: 25,
                    20: 52}

    def test_hodges_lehmann_is_the_median_of_walsh_averages(self):
        # Walsh averages of (1, 2, 4): 1, 1.5, 2.5, 2, 3, 4 -> median (2 + 2.5) / 2.
        self.assertEqual(csr.walsh_averages([1, 2, 4]), [1, 1.5, 2, 2.5, 3, 4])
        self.assertEqual(csr.hodges_lehmann([1, 2, 4]), 2.25)
        self.assertEqual(csr.hodges_lehmann([3.0]), 3.0)
        # One outlier moves the mean (1.81) far more than the estimate: the 8th of the 15
        # sorted Walsh averages of (-0.1, 0, 0.05, 0.1, 9) is 0.05.
        self.assertAlmostEqual(csr.hodges_lehmann([0.0, 0.1, -0.1, 0.05, 9.0]), 0.05)
        with self.assertRaises(ValueError):
            csr.hodges_lehmann([])

    def test_signed_rank_null_matches_the_published_critical_values(self):
        for n, critical in self.CRITICAL_005.items():
            counts = csr.signed_rank_counts(n)
            self.assertEqual(sum(counts), 2 ** n)
            self.assertEqual(counts, counts[::-1], "the null is symmetric")
            below = sum(counts[:critical + 1])
            self.assertLessEqual(below / 2 ** n, 0.025, n)
            self.assertGreater((below + counts[critical + 1]) / 2 ** n, 0.025, n)
        self.assertEqual(csr.signed_rank_counts(3), [1, 1, 1, 2, 1, 1, 1])

    def test_wilcoxon_interval_takes_the_kth_walsh_averages(self):
        values = [0.31, -0.12, 0.08, 0.44, 0.27, -0.05, 0.19, 0.36, 0.02, 0.23]
        walsh = csr.walsh_averages(values)
        low, high, coverage = csr.wilcoxon_interval(values)
        # n = 10: c = 8 (P(T+ <= 8) = 25/1024), so k = 9 of M = 55.
        self.assertEqual((low, high), (walsh[8], walsh[46]))
        self.assertAlmostEqual(coverage, 1 - 50 / 1024)
        six = [0.5, 0.1, 0.4, 0.2, 0.6, 0.3]
        self.assertEqual(csr.wilcoxon_interval(six), (0.1, 0.6, 1 - 2 / 64))
        self.assertIsNone(csr.wilcoxon_interval(six[:5]), "no 95% interval from 5 pairs")

    def test_confidence_sequence_covers_under_optional_stopping(self):
        # 1,000 simulated runs of up to 60 pairs, looked at after every pair. A run fails
        # when the true median leaves the running sequence at any look. Ville's inequality
        # bounds that probability by alpha = 0.05.
        rng = __import__("random").Random(20261022)
        failures = 0
        for run_index in range(1000):
            if run_index % 2:
                true_median = -0.4
                values = [true_median + rng.gauss(0.0, 0.15) for _ in range(60)]
            else:
                # Skewed: an exponential shifted so its median is the target.
                true_median = 0.1
                values = [true_median - __import__("math").log(2) / 3 + rng.expovariate(3.0)
                          for _ in range(60)]
            sequence = csr.median_confidence_sequence(values)
            if sequence is None or not sequence[0] <= true_median <= sequence[1]:
                failures += 1
        self.assertGreaterEqual(1 - failures / 1000, 0.95, failures)

    def test_sequence_needs_seven_unanimous_pairs_to_exclude_at_alpha_005(self):
        # The e-value of n unanimous signs is C(2n, n) / 2**n: 14.4 at n = 6, 26.8 at n = 7.
        below = [-1.0 - 0.01 * i for i in range(7)]
        self.assertEqual(csr.median_confidence_sequence(below[:6]), (float("-inf"), float("inf")))
        # At 7 neither unanimous count is accepted: the sequence is [min, max].
        self.assertEqual(csr.median_confidence_sequence(below), (-1.06, -1.0))

    def test_decision_reports_below_above_and_within_tolerance(self):
        clear = [-1.0 - 0.01 * i for i in range(8)]  # ratio about 0.36
        verdicts, decided = csr.sequential_decision(clear)
        self.assertEqual(verdicts, {"0.5x": "below", "1x": "below"})
        self.assertTrue(decided)
        rng = __import__("random").Random(5)
        near_parity = [rng.uniform(-0.01, 0.01) for _ in range(200)]
        verdicts, decided = csr.sequential_decision(near_parity)
        self.assertEqual(verdicts, {"0.5x": "above", "1x": "within 5%"})
        self.assertTrue(decided)
        verdicts, decided = csr.sequential_decision(near_parity[:5])
        self.assertFalse(decided)
        self.assertEqual(csr.sequential_decision([]), ({"0.5x": None, "1x": None}, False))

    def run_sequential(self, portal_s, reference_s, max_pairs, reps=1):
        clock = FakeClock()

        def runner(seconds):
            def call(scene, rep):
                clock.now += seconds[rep % len(seconds)]
                return 0, ""
            return call

        collect = Collect()
        runners = {"portal": runner(portal_s), "reference": runner(reference_s)}
        original = csr.time.monotonic
        csr.time.monotonic = clock
        try:
            finished = csr.measure_scene(("m.py", "A"), reps, runners, collect, collect.done,
                                         collect.lock, max_pairs=max_pairs)
        finally:
            csr.time.monotonic = original
        return finished, collect.records

    def test_sequential_lane_stops_early_on_a_clear_answer(self):
        finished, records = self.run_sequential([1.0], [3.0], max_pairs=40)
        self.assertTrue(finished)
        self.assertEqual(len(records), 14, "seven unanimous pairs decide both thresholds")
        self.assertEqual([r["order"] for r in records][:4], ["AB", "AB", "BA", "BA"])

    def test_sequential_lane_runs_long_on_a_close_answer(self):
        # Alternating 0.9x and 1.1x pairs straddle parity and stay wider than +-5%.
        finished, records = self.run_sequential([0.9, 1.1], [1.0], max_pairs=12)
        self.assertTrue(finished)
        self.assertEqual(len(records), 24, "an undecided scene runs to --max-pairs")

    def test_calibration_report_rows_and_instruction_ratios(self):
        json = __import__("json")
        runs = []
        for rep, (portal, reference) in enumerate([(1.0, 2.0), (1.1, 2.0), (0.9, 2.1)]):
            order = "AB" if rep % 2 == 0 else "BA"
            runs += [dict(run("m.py", "P", "portal", rep, portal), order=order,
                          instructions=100 + rep, load_start=1.0, load_end=2.0),
                     dict(run("m.py", "P", "reference", rep, reference), order=order,
                          instructions=200, load_start=3.0, load_end=4.0)]
            runs += [dict(run("m.py", "C", "portal", rep, portal), order=order),
                     dict(run("m.py", "C", "reference", rep, reference), order=order)]
        runs += [dict(run("m.py", "C", "portal", "instructions", 40.0), kind="instructions",
                      instructions=50),
                 dict(run("m.py", "C", "reference", "instructions", 80.0), kind="instructions",
                      instructions=200)]
        pairs, summaries = csr.scene_calibration(runs)
        p_rows = [row for row in pairs if row["scene"] == "P"]
        self.assertEqual([row["pair_index"] for row in p_rows], [0, 1, 2])
        # pair 1 ran B first, so its load window runs from the Reference's start to the
        # portal's end
        self.assertEqual((p_rows[1]["order"], p_rows[1]["load_start"], p_rows[1]["load_end"]),
                         ("BA", 3.0, 2.0))
        self.assertAlmostEqual(p_rows[0]["log_ratio"], __import__("math").log(0.5))
        self.assertEqual(summaries[("m.py", "P")]["instruction_source"], "perf stat on the timed runs")
        self.assertAlmostEqual(summaries[("m.py", "P")]["instruction_ratio"], 101 / 200)
        self.assertEqual(summaries[("m.py", "C")]["instruction_ratio"], 0.25)
        self.assertIsNone(summaries[("m.py", "C")]["ci_low"], "no Wilcoxon interval from 3 pairs")
        # Instruction runs are 30x slower and never enter the timed ratios.
        included, _ = csr.scene_ratios(runs)
        self.assertEqual(included[("m.py", "C")]["pairs"], 3)
        with tempfile.TemporaryDirectory() as tmp:
            out = pathlib.Path(tmp)
            (out / "runs.ndjson").write_text("".join(json.dumps(r) + "\n" for r in runs))
            csr.write_dashboard(out, out / "dash.md")
            written = [json.loads(line) for line in (out / "pairs.ndjson").read_text().splitlines()]
            self.assertEqual(len(written), 6)
            self.assertTrue(all(row["qualification"] == "calibration-unqualified" for row in written))
            self.assertIn("Paired calibration (calibration-unqualified", (out / "dash.md").read_text())

    def test_instrument_parsers_and_argv(self):
        self.assertEqual(csr.parse_perf_instructions("8217949418,,instructions:u,1408290454,100.00,,"),
                         8217949418)
        self.assertIsNone(csr.parse_perf_instructions("<not supported>,,instructions:u,0,0.00,,"))
        self.assertIsNone(csr.parse_perf_instructions(""))
        with tempfile.TemporaryDirectory() as tmp:
            stem = pathlib.Path(tmp) / "portal-0"
            argv, env, read = csr.instrument(["python", "x.py"], {"A": "1"}, "cachegrind", stem)
            self.assertEqual(argv[:4], ["valgrind", "--tool=cachegrind", "--cache-sim=no",
                                        "--trace-children=yes"])
            self.assertEqual(argv[-2:], ["python", "x.py"])
            self.assertEqual(env, {"A": "1", "PYTHONHASHSEED": "0", "OMP_NUM_THREADS": "1",
                                   "OPENBLAS_NUM_THREADS": "1", "MKL_NUM_THREADS": "1",
                                   "LP_NUM_THREADS": "1"})
            out_dir = pathlib.Path(argv[4].split("=", 1)[1]).parent
            self.assertIsNone(read(), "no files yet")
            (out_dir / "cg.11").write_text("events: Ir\nsummary: 1000\n")
            (out_dir / "cg.12").write_text("cmd: /sbin/ldconfig -p\nevents: Ir\nsummary: 25\n")
            self.assertEqual(read(), 1025, "children count toward the run")
            argv, env, _ = csr.instrument(["python"], {}, "perf", stem)
            self.assertEqual(argv[:5], ["perf", "stat", "-x", ",", "-e"])
            self.assertEqual(csr.instrument(["python"], {}, None, stem)[:2], (["python"], {}))


if __name__ == "__main__":
    unittest.main(verbosity=1)
