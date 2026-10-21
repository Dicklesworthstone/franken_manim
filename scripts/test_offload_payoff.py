#!/usr/bin/env python3
"""Tests for scripts/offload_payoff.py (fm-offload-payoff-4rve): the prediction, end to end."""
import json
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import offload_payoff as op  # noqa: E402


def segment(klass, wall, reasons=()):
    return {"class": klass, "wall_s": wall, "reasons": list(reasons)}


class Prediction(unittest.TestCase):
    def test_offload_scales_native_segments_and_keeps_python_and_outside_time(self):
        segments = [segment("native", 4.0), segment("rate-table", 2.0),
                    segment("python", 3.0, ["updaters:1"]),
                    segment("python", 1.0, ["library-updaters:1"])]
        # portal 12 s: 10 s in segments, 2 s outside; Reference 10 s; r = 0.5.
        p = op.predict(12.0, 10.0, segments, 0.5)
        self.assertAlmostEqual(p["current"], 1.2)
        self.assertAlmostEqual(p["strict"], (2.0 + 0.5 * 6.0 + 4.0) / 10.0)
        self.assertAlmostEqual(p["optimistic"], (2.0 + 0.5 * 7.0 + 3.0) / 10.0)

    def test_library_only_needs_every_reason_to_be_a_library_updater(self):
        self.assertTrue(op.library_only(segment("python", 1, ["library-updaters:2"])))
        self.assertFalse(op.library_only(segment("python", 1, ["library-updaters:1",
                                                                "updaters:1"])))
        self.assertFalse(op.library_only(segment("native", 1)))

    def test_measured_outside_time_never_goes_negative(self):
        # A census can overrun the process clock by rounding; outside time clamps at zero.
        p = op.predict(1.0, 1.0, [segment("python", 1.2, ["stop-condition"])], 0.5)
        self.assertAlmostEqual(p["strict"], 1.2)

    def test_a_later_run_replaces_a_scene_whole(self):
        with tempfile.TemporaryDirectory() as tmp:
            first, rerun = pathlib.Path(tmp) / "first", pathlib.Path(tmp) / "rerun"
            for run_dir, records in (
                    (first, [("A", "portal", None, 300.0), ("A", "reference", 0, 20.0),
                             ("B", "portal", 0, 4.0), ("B", "reference", 0, 2.0)]),
                    (rerun, [("A", "portal", 0, 900.0), ("A", "reference", 0, 21.0)])):
                run_dir.mkdir()
                (run_dir / "runs.ndjson").write_text("".join(
                    json.dumps({"module": "m.py", "scene": s, "engine": e, "rep": 0, "exit": x,
                                "seconds": t}) + "\n" for s, e, x, t in records))
            runs, owner = op.merged_runs([first, rerun])
        a_runs = sorted((r["engine"], r["seconds"]) for r in runs if r["scene"] == "A")
        self.assertEqual(a_runs, [("portal", 900.0), ("reference", 21.0)])
        self.assertEqual(owner[("m.py", "A")], rerun)
        self.assertEqual(owner[("m.py", "B")], first)
        self.assertEqual(len(runs), 4)

    def test_report_from_a_synthetic_run(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = pathlib.Path(tmp)
            run = tmp / "run"
            (run / "work" / op.slug("m.py", "A")).mkdir(parents=True)
            (run / "header.json").write_text(json.dumps({"mode": "frames", "reps": 1}))
            runs = [{"module": "m.py", "scene": s, "engine": e, "rep": 0, "exit": 0,
                     "seconds": t} for s, e, t in (("A", "portal", 10.0), ("A", "reference", 10.0),
                                                   ("B", "portal", 4.0), ("B", "reference", 2.0))]
            (run / "runs.ndjson").write_text("".join(json.dumps(r) + "\n" for r in runs))
            (run / "work" / op.slug("m.py", "A") / "census-0.ndjson").write_text(
                json.dumps(segment("native", 8.0)) + "\n")
            same = tmp / "same.json"
            same.write_text(json.dumps(
                [{"engine": "native", "exit": 0, "wall_s": 1.0, "cpu_s": 1.0, "load1": 1.0},
                 {"engine": "portal", "exit": 0, "wall_s": 4.0, "cpu_s": 4.0, "load1": 1.0}]))
            out = tmp / "payoff.md"
            result = op.write_report(type("Args", (), {
                "runs": [run], "portal_scenes": 4, "same_scene": [f"{same}:one"],
                "ratio_from": "one", "out": out})())
            text = out.read_text()
        self.assertEqual(result["census_scenes"], 2)
        self.assertAlmostEqual(result["r"], 0.25)
        # A: (2 + 0.25 * 8) / 10 = 0.4; B has no segments: 4 / 2 = 2.0; median 1.2.
        self.assertTrue(result["strict"].startswith("1.20"))
        self.assertIn("2 of the 4 corpus scenes", text)
        self.assertIn("| native | 1 | 8.0 | 100.0% |", text)
        self.assertIn("| scenes at <= 0.50x (PG-1, G2) | 0 | 1 | 1 |", text)


if __name__ == "__main__":
    unittest.main()
