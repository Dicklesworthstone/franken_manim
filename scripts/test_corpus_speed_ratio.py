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
        self.records, self.done, self.lock = [], set(), threading.Lock()

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

    def test_runs_interleave_engines_in_the_log(self):
        self.assertEqual(csr.interleaved_plan([("m.py", "A")], 2),
                         [(("m.py", "A"), 0, "portal"), (("m.py", "A"), 0, "reference"),
                          (("m.py", "A"), 1, "portal"), (("m.py", "A"), 1, "reference")])
        collect = Collect()
        runners = {engine: (lambda scene, rep: (0, "")) for engine in csr.ENGINES}
        csr.measure_scene(("m.py", "A"), 3, runners, collect, collect.done, collect.lock)
        self.assertEqual([(r["engine"], r["rep"]) for r in collect.records],
                         [("portal", 0), ("reference", 0), ("portal", 1), ("reference", 1),
                          ("portal", 2), ("reference", 2)])
        self.assertTrue(all(r["label"] == csr.LABEL for r in collect.records))

    def test_resume_skips_finished_runs_and_failure_stops_the_scene(self):
        collect = Collect()
        collect.done.add(("m.py", "A", "portal", 0))
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
        self.assertIn("| m.py:D | portal:timeout | stuck \\| in a loop |", text)


if __name__ == "__main__":
    unittest.main(verbosity=1)
