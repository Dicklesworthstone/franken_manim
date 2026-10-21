"""Unit tests for scripts/portal_witnesses.py (fm-5wq.12), on synthetic modules."""
import pathlib
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

    def test_top_level_statements_of_a_script_suite_are_separate_identities(self):
        rows = self.rows()
        recorder = pw.Recorder(pw.Probes(rows), "suite_under_test.py", render_rows={0})
        code = compile("mod.Square().width()\nmod.area(mod.Circle())\n", "suite_under_test.py",
                       "exec")
        with recorder:
            exec(code, {"mod": self.mod})
        hits = {recorder.spell(key): {rows[i]["symbol"] for i in found}
                for key, found in recorder.hits.items()}
        self.assertEqual(hits, {"<module>:L1": {"Square.width", "Square"},
                                "<module>:L2": {"Circle.width", "area"}})
        # Square.width is a render marker here: only the first statement rendered.
        self.assertEqual({recorder.spell(key) for key in recorder.renders}, {"<module>:L1"})

    def test_a_location_disabled_under_one_statement_comes_back_for_the_next(self):
        rows = self.rows()
        recorder = pw.Recorder(pw.Probes(rows), "suite_under_test.py", render_rows=set())
        code = compile("for _ in range(10): mod.Square().width()\nmod.Circle().width()\n",
                       "suite_under_test.py", "exec")
        with recorder:
            exec(code, {"mod": self.mod})
        hits = {recorder.spell(key): {rows[i]["symbol"] for i in found}
                for key, found in recorder.hits.items()}
        # width is disabled after IDENTITY_CALLS calls in the loop, and restarted by line 2.
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
        ''')
        self.assertEqual(pw.gate_suites(script),
                         [("alpha", "runpy"), ("beta", "runpy"), ("gamma", "main")])
        with self.assertRaises(ValueError):
            pw.gate_suites("python3 crates/fmn-python/tests/gamma.py\n")

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
