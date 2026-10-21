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
        sys.setprofile(recorder)
        try:
            body()
        finally:
            sys.setprofile(None)
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
        sys.setprofile(recorder)
        try:
            exec(code, {"mod": self.mod})
        finally:
            sys.setprofile(None)
        hits = {identity: {rows[i]["symbol"] for i in found}
                for identity, found in recorder.hits.items()}
        self.assertEqual(hits, {"<module>:L1": {"Square.width", "Square"},
                                "<module>:L2": {"Circle.width", "area"}})
        # Square.width is a render marker here: only the first statement rendered.
        self.assertEqual(recorder.renders, {"<module>:L1"})

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
        self.assertEqual(pw.gate_suites(script), ["alpha", "beta", "gamma"])
        with self.assertRaises(ValueError):
            pw.gate_suites("python3 crates/fmn-python/tests/gamma.py\n")

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
