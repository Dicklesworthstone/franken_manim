"""Unit tests for scripts/portal_witnesses.py (fm-5wq.12), on synthetic modules."""
import pathlib
import runpy
import sys
import tempfile
import textwrap
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import portal_witnesses as pw  # noqa: E402

SYNTHETIC = textwrap.dedent('''
    class Shape:
        def width(self):
            return 2.0

    class Square(Shape):
        def __init__(self):
            self.side = 1.0

    class Circle(Shape):
        def __init__(self):
            self.radius = 1.0

    def refused(*args, **kwargs):
        raise NotImplementedError

    refused._fmn_schema_placeholder = True

    def area(shape):
        return shape.width() ** 2

    class Disc(Shape):
        def width(self):
            return 3.0
''')


def row(symbol, kind, status="same", module="fmn_witness_synthetic"):
    return {"module": module, "symbol": symbol, "kind": kind, "status": status}


class Harness(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        (pathlib.Path(cls.tmp.name) / "fmn_witness_synthetic.py").write_text(SYNTHETIC)
        sys.path.insert(0, cls.tmp.name)
        import fmn_witness_synthetic
        cls.mod = fmn_witness_synthetic

    @classmethod
    def tearDownClass(cls):
        sys.path.remove(cls.tmp.name)
        cls.tmp.cleanup()

    def rows(self):
        return [row("Square.width", "method"), row("Circle.width", "method"),
                row("Square", "class"), row("refused", "function"), row("area", "function"),
                row("Shape.width", "method", status="excluded")]

    def record(self, body):
        rows = self.rows()
        probes = pw.Probes(rows)
        recorder = pw.Recorder(probes, __file__, render_rows=set())
        with recorder:
            body()
        return rows, probes, recorder

    def test_a_method_counts_for_the_rows_whose_class_the_instance_is(self):
        # Defaults, not closures: a closure over self would make the frame a TestCase's.
        def test_squares(mod=self.mod):
            mod.Square().width()
        rows, _, recorder = self.record(test_squares)
        hit = {rows[i]["symbol"] for i in recorder.hits["test_squares"]}
        # The Square's width call is Square.width's, not Circle.width's, and the class row
        # counts because a Square was initialized.
        self.assertEqual(hit, {"Square.width", "Square"})

    def test_a_class_row_needs_its_exact_class_and_functions_count_by_code(self):
        def test_circle_area(mod=self.mod):
            mod.area(mod.Circle())
        rows, _, recorder = self.record(test_circle_area)
        hit = {rows[i]["symbol"] for i in recorder.hits["test_circle_area"]}
        self.assertEqual(hit, {"Circle.width", "area"})

    def test_calls_outside_a_test_function_belong_to_the_module(self):
        # No frame of the suite file's <module> is on the stack here, so the plain identity.
        rows, _, recorder = self.record(lambda: self.mod.Square().width())
        self.assertEqual({rows[i]["symbol"] for i in recorder.hits["<module>"]},
                         {"Square.width", "Square"})

    def run_script_suite(self, source, render_rows):
        """A script suite run the way the harness runs one: runpy over its file."""
        rows = self.rows()
        suite = pathlib.Path(self.tmp.name) / "suite_under_test.py"
        suite.write_text(source)
        recorder = pw.Recorder(pw.Probes(rows), str(suite), render_rows=render_rows)
        with recorder:
            runpy.run_path(str(suite), init_globals={"mod": self.mod})
        return rows, recorder

    def test_top_level_statements_of_a_script_suite_are_separate_identities(self):
        rows, recorder = self.run_script_suite("mod.Square().width()\nmod.area(mod.Circle())\n",
                                               render_rows={0})
        hits = {recorder.spell(key): {rows[i]["symbol"] for i in found}
                for key, found in recorder.hits.items()}
        self.assertEqual(hits, {"<module>:L1": {"Square.width", "Square"},
                                "<module>:L2": {"Circle.width", "area"}})
        # Square.width is a render marker here: only the first statement rendered.
        self.assertEqual({recorder.spell(key) for key in recorder.renders}, {"<module>:L1"})

    def test_a_loop_on_one_line_is_one_statement_and_the_next_line_another(self):
        rows, recorder = self.run_script_suite(
            "for _ in range(10): mod.Square().width()\nmod.Circle().width()\n", render_rows=set())
        hits = {recorder.spell(key): {rows[i]["symbol"] for i in found}
                for key, found in recorder.hits.items()}
        # The ten iterations repeat line 1's LINE event without making new identities.
        self.assertEqual(hits, {"<module>:L1": {"Square.width", "Square"},
                                "<module>:L2": {"Circle.width"}})

    def test_placeholders_and_unwitnessed_statuses_are_never_probed(self):
        rows, probes, recorder = self.record(lambda: None)
        index = {r["symbol"]: i for i, r in enumerate(rows)}
        self.assertEqual(probes.status[index["refused"]], "placeholder")
        self.assertNotIn(index["Shape.width"], probes.status)
        self.assertEqual(probes.status[index["area"]], "ok")

    def test_a_failed_test_witnesses_nothing_and_renders_are_counted_separately(self):
        rows = self.rows()
        probes = pw.Probes(rows)
        records = [
            {"suite": "s", "test": "T.test_a", "passed": True, "renders": True, "rows": [0]},
            {"suite": "s", "test": "T.test_b", "passed": False, "renders": False, "rows": [1]},
            {"suite": "s", "summary": True, "cases": 2, "failed": ["T.test_b"], "error": None},
        ]
        table = {r["symbol"]: r for r in pw.registry(rows, probes, records)}
        self.assertEqual((table["Square.width"]["semantic"], table["Square.width"]["rendered"]),
                         (1, 1))
        self.assertEqual(table["Circle.width"]["semantic"], 0)
        self.assertEqual(table["Square.width"]["example"], "s::T.test_a")
        self.assertNotIn("Shape.width", table)

    def test_an_override_witnesses_the_base_row_it_answers_for(self):
        # CoordinateSystem.c2p is answered by Axes.c2p: calling the name on an instance of the
        # row's class is what the row means, whichever override runs.
        rows = [row("Shape.width", "method"), row("Disc", "class")]
        recorder = pw.Recorder(pw.Probes(rows), __file__, render_rows=set())

        def test_disc(mod=self.mod):
            mod.Disc().width()
        with recorder:
            test_disc()
        self.assertEqual({rows[i]["symbol"] for i in recorder.hits["test_disc"]},
                         {"Shape.width", "Disc"})

    def test_an_installer_wrapper_with_star_args_still_names_its_receiver(self):
        rows = [row("Square.width", "method")]
        recorder = pw.Recorder(pw.Probes(rows), __file__, render_rows=set())
        original = self.mod.Square.width

        def install():
            def width(*args, **kwargs):
                return original(*args, **kwargs)
            return width
        self.mod.Square.width = install()
        try:
            with recorder:
                self.mod.Square().width()
        finally:
            del self.mod.Square.width
        self.assertEqual(recorder.hits["<module>"], {0})

    def test_a_mutant_counts_only_the_results_it_changed(self):
        rows = self.rows()
        probes = pw.Probes(rows)
        pw.MUTANT_CALLS["applied"] = 0
        pw.mutate(probes, 1, "shifted")  # Circle.width returns a float, not an array
        try:
            self.assertEqual(self.mod.Circle().width(), 2.0)
            self.assertEqual(pw.MUTANT_CALLS["applied"], 0)
        finally:
            del self.mod.Circle.width

    def test_a_trivial_mutant_replaces_the_implementation(self):
        rows = self.rows()
        probes = pw.Probes(rows)
        pw.mutate(probes, 0, "trivial")
        try:
            self.assertIsNone(self.mod.Square().width())
            self.assertEqual(self.mod.Circle().width(), 2.0)
        finally:
            del self.mod.Square.width

    def test_the_gate_suite_list_is_the_loop_then_the_direct_runs(self):
        script = textwrap.dedent('''
            for suite in alpha beta; do
                python3 - "$suite" <<'PY'
            PY
            done
            python3 crates/fmn-python/tests/gamma.py
            python3 crates/fmn-python/tests/alpha.py
            FMN_REQUIRE_FFMPEG=1 FMN_X= python3 crates/fmn-python/tests/delta.py
            # python3 crates/fmn-python/tests/commented.py
        ''')
        self.assertEqual(pw.gate_suites(script),
                         [("alpha", "runpy"), ("beta", "runpy"), ("gamma", "main"), ("delta", "main")])
        with self.assertRaises(ValueError):
            pw.gate_suites("python3 crates/fmn-python/tests/gamma.py\n")

    def test_each_registry_failure_class_fails_the_check(self):
        # Negative controls for check's non-mutant failure classes, one planted defect each.
        rows = [r for r in self.rows() if r["symbol"] != "refused"]
        probes = pw.Probes(rows)
        area = next(i for i, r in enumerate(rows) if r["symbol"] == "area")
        passing = [{"suite": "s", "test": "T.test_a", "passed": True, "renders": False,
                    "rows": [0, area]},
                   {"suite": "s", "summary": True, "unittest": True, "cases": 1, "failed": [],
                    "error": None}]
        table = pw.registry(rows, probes, passing)
        committed = pw.read_registry(pw.write_registry(table, passing))
        self.assertEqual(pw.registry_problems(table, passing, committed), [])
        # A witness lost: the same suite now invokes only area.
        lost = [dict(passing[0], rows=[area]), passing[1]]
        self.assertEqual(
            pw.registry_problems(pw.registry(rows, probes, lost), lost, committed),
            ["fmn_witness_synthetic.Square.width: lost every semantic witness"])
        # A suite that defines tests and runs none.
        skipped = [dict(passing[1], cases=0)]
        self.assertIn("s: defines tests but executed none",
                      pw.registry_problems(table, skipped, {}))
        # A same/improved symbol that resolves to a placeholder (planted in the synthetic module).
        planted = rows + [row("refused", "function")]
        problems = pw.registry_problems(pw.registry(planted, pw.Probes(planted), passing),
                                        passing, {})
        self.assertEqual(problems, ["fmn_witness_synthetic.refused: same in the ledger, but it "
                                    "resolves to placeholder"])

    def test_the_installed_wheel_gate_runs_the_witness_check(self):
        # The registry is only honest while the gate both runs its suites and checks them.
        script = pw.GATE.read_text(encoding="utf-8")
        self.assertGreater(len(pw.gate_suites(script)), 100)
        self.assertIn("scripts/portal_witnesses.py check", script)

    def test_the_ledger_parser_keeps_manimlib_rows_and_needs_its_header(self):
        text = ("# preamble\n# module\tsymbol\tcanonical\tkind\texported\tsignature_defaults\t"
                "status\tevidence\ttests\tnotes\n"
                "fmn.cli\tbatch\tbatch\tcli_command\t0\t-\timproved\tBN-15\tx\ty\n"
                "manimlib\tCircle\tCircle\tclass\t1\t-\tsame\t-\tx\ty\n")
        parsed = pw.ledger_rows(text)
        self.assertEqual([(r["symbol"], r["status"]) for r in parsed], [("Circle", "same")])
        with self.assertRaises(ValueError):
            pw.ledger_rows("manimlib\tCircle\n")


if __name__ == "__main__":
    unittest.main()
