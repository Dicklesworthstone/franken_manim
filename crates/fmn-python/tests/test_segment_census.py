"""The segment census (fm-offload-payoff-4rve): its classifier, and its records from real scenes.

The end-to-end case renders through the installed wheel in a subprocess, because the census
installs while manimlib initializes, and only when FMN_SEGMENT_CENSUS is set at that moment.
"""
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import textwrap
import unittest

from fmn_python.segment_census import SCHEMA, VERSION, _has_rate_table, classify

SCENES = textwrap.dedent('''
    import sys
    import manimlib as m

    class Wobble(m.Animation):
        def interpolate_mobject(self, alpha):
            self.mobject.set_opacity(1 - alpha / 2)

    class Segments(m.Scene):
        def construct(self):
            square, circle = m.Square(), m.Circle()
            self.play(m.FadeIn(square))
            self.play(square.animate.shift(m.RIGHT), rate_func=lambda t: t * t)
            self.play(m.FadeOut(square, rate_func=lambda t: t ** 3))
            self.play(Wobble(square, rate_func=lambda t: t ** 3))
            square.add_updater(lambda mob, dt: mob.rotate(dt))
            self.play(m.FadeIn(circle))
            square.clear_updaters()
            self.play(Wobble(circle))
            self.wait(0.25)
            self.wait(0.25, stop_condition=lambda: False)

    class Hooked(m.Scene):
        def update_mobjects(self, dt):
            super().update_mobjects(dt)

        def construct(self):
            self.play(m.FadeIn(m.Square()))

    class Selecting(m.InteractiveScene):
        def construct(self):
            self.play(m.FadeIn(m.Square()))

    for scene in (Segments, Hooked, Selecting):
        scene().render(sys.argv[1] + "/" + scene.__name__, resolution=(64, 36), fps=8, threads=1)
''')


class Classifier(unittest.TestCase):
    def test_classes_and_every_demotion_reason(self):
        self.assertEqual(classify([], 0, False, False, False), ("native", []))
        self.assertEqual(classify([], 0, False, False, True), ("rate-table", []))
        self.assertEqual(
            classify(["Wobble", "Shake", "Wobble"], 2, True, True, True),
            ("python", ["python-animation:Shake", "python-animation:Wobble", "updaters:2",
                        "authored-hook", "stop-condition"]))
        self.assertEqual(classify([], 1, False, False, False, 2),
                         ("python", ["updaters:1", "library-updaters:2"]))
        for args, reason in (((["W"], 0, False, False, False), "python-animation:W"),
                             (([], 0, False, False, False, 1), "library-updaters:1"),
                             (([], 1, False, False, False), "updaters:1"),
                             (([], 0, True, False, False), "authored-hook"),
                             (([], 0, False, True, False), "stop-condition")):
            self.assertEqual(classify(*args), ("python", [reason]))

    def test_rate_tables_are_found_in_specs_and_nested_members(self):
        plain = ("transform", None, None, 1.0, "smooth", None, {})
        table = ("transform", None, None, 1.0, [0.0, 0.5, 1.0], None, {})
        group = ("animation_group", None, None, 1.0, None, None, {"members": [plain, table]})
        self.assertFalse(_has_rate_table([plain]))
        self.assertTrue(_has_rate_table([table]))
        self.assertTrue(_has_rate_table([group]))
        self.assertFalse(_has_rate_table([("transform", None)]))


class RealScenes(unittest.TestCase):
    def test_every_segment_is_recorded_and_classified(self):
        with tempfile.TemporaryDirectory(prefix="fmn-census-") as work:
            work = pathlib.Path(work)
            census = work / "census.ndjson"
            (work / "scenes.py").write_text(SCENES, encoding="utf-8")
            env = dict(os.environ, FMN_SEGMENT_CENSUS=str(census))
            result = subprocess.run([sys.executable, str(work / "scenes.py"), str(work)], env=env,
                                    capture_output=True, text=True, timeout=600)
            self.assertEqual(result.returncode, 0, result.stderr[-2000:])
            records = [json.loads(line) for line in census.read_text().splitlines()]
        self.assertTrue(all(r["schema"] == SCHEMA and r["version"] == VERSION for r in records))
        segments = [(r["scene"], r["kind"], r["class"], r["reasons"]) for r in records]
        self.assertEqual(segments, [
            ("Segments", "play", "native", []),
            ("Segments", "play", "rate-table", []),
            ("Segments", "play", "rate-table", []),
            ("Segments", "play", "python", ["python-animation:Wobble"]),
            ("Segments", "play", "python", ["updaters:1"]),
            ("Segments", "play", "python", ["python-animation:Wobble"]),
            ("Segments", "wait", "native", []),
            ("Segments", "wait", "python", ["stop-condition"]),
            ("Hooked", "play", "python", ["authored-hook"]),
            ("Selecting", "play", "python", ["library-updaters:1"]),
        ])
        self.assertEqual([r["index"] for r in records], [0, 1, 2, 3, 4, 5, 6, 7, 0, 0])
        for record in records:
            self.assertGreater(record["wall_s"], 0)
            self.assertGreaterEqual(record["python_callback_s"], 0)
            self.assertIn("updater_call", record["crossings"])
        # A Python updater and a Python animation cross into Python every frame.
        self.assertGreater(records[4]["crossings"]["updater_call"], 0)
        self.assertGreater(records[5]["crossings"]["method_dispatch"], 0)

    def test_only_the_variable_installs_it(self):
        probe = "import manimlib as m; print(getattr(m, '_FMN_SEGMENT_CENSUS_INSTALLED', False))"
        base = {k: v for k, v in os.environ.items() if k != "FMN_SEGMENT_CENSUS"}
        for env, expected in ((base, "False"), (dict(base, FMN_SEGMENT_CENSUS=os.devnull), "True")):
            result = subprocess.run([sys.executable, "-c", probe], env=env, capture_output=True,
                                    text=True, timeout=300)
            self.assertEqual(result.returncode, 0, result.stderr[-2000:])
            self.assertEqual(result.stdout.strip(), expected)


if __name__ == "__main__":
    unittest.main()
