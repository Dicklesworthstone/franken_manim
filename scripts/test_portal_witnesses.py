"""Unit tests for scripts/portal_witnesses.py (fm-5wq.12), on synthetic modules."""
import pathlib
import gc
import runpy
import sys
import tempfile
import textwrap
import unittest
import weakref
from unittest import mock

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

    class Span(Shape):
        @staticmethod
        def contains(outer, inner):
            return outer[0] <= inner[0] and inner[1] <= outer[1]

    import functools

    def _install_area(cls):
        def tile_area(self):
            return self.width() ** 2
        tile_area.__name__ = "area"
        tile_area.__qualname__ = cls.__qualname__ + ".area"
        cls.area = tile_area

    class Tile(Shape):
        pass

    _install_area(Tile)

    def _getter(attribute):
        def get(self):
            return getattr(self, attribute)
        return get

    class Box(Shape):
        side = 1.0
        depth = 2.0

    Box.get_side = _getter("side")
    Box.get_depth = _getter("depth")

    @functools.lru_cache(maxsize=None)
    def cached_area(side):
        return side * side

    def maybe_area(side):
        if side < 0:
            raise ValueError("negative side")
        return side * side

    import abc

    class Template(abc.ABC):
        @abc.abstractmethod
        def width(self):
            ...

    class Sealed:
        def __init__(self):
            raise NotImplementedError("sealed")

    def delayed_area(side):
        yield side
        if side < 0:
            raise ValueError("negative side")
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
        # The drill reruns this suite in one interpreter: the next run imports
        # its own copy rather than this one, whose directory is about to go.
        sys.modules.pop("fmn_witness_synthetic", None)
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

    def trace_runner_fixture(self, name, entry, mode="main", plain=False):
        root = pathlib.Path(self.tmp.name)
        (root / "Cargo.toml").write_text('[workspace.package]\nversion = "0.5.0"\n')
        ledger = root / "runner_ledger.tsv"
        ledger.write_text("runner fixture: rows supplied below\n")
        source = textwrap.dedent('''
            import pickle
            import unittest
            import fmn_witness_synthetic as witness

            class Sample:
                value = 7

            class RunnerCase(unittest.TestCase):
                def test_pickle(self):
                    witness.runner_calls.append("pickle")
                    self.assertEqual(witness.Square().width(), 2)
                    self.assertEqual(pickle.loads(pickle.dumps(Sample())).value, 7)
        ''') + entry
        (root / (name + ".py")).write_text(source)
        original_main, original_runner = unittest.main, unittest.TextTestRunner.run
        original_factory = unittest.TextTestRunner._makeResult
        with (mock.patch.object(pw, "ROOT", root), mock.patch.object(pw, "LEDGER", ledger),
              mock.patch.object(pw, "TESTS", root),
              mock.patch.object(pw, "ledger_rows", return_value=self.rows()[:3]),
              mock.patch.object(self.mod, "runner_calls", [], create=True)):
            try:
                records = pw.trace_suite(name, mode, None, plain=plain)
                calls = list(self.mod.runner_calls)
                restored = (unittest.main is original_main,
                            unittest.TextTestRunner.run is original_runner,
                            unittest.TextTestRunner._makeResult is original_factory)
            finally:
                # The pre-fix implementation leaks its patched unittest.main.
                # Keep this planted red control from contaminating other tests.
                unittest.main, unittest.TextTestRunner.run = original_main, original_runner
                unittest.TextTestRunner._makeResult = original_factory
                sys.modules.pop(name, None)
        return records, calls, restored

    def test_a_failure_only_under_the_harness_is_rerun_plain_and_kept_apart(self):
        # fm-6l60: protocol suites can depend on the real portal never having initialized,
        # which the harness's ledger import breaks. Planted here as a test that fails while a
        # profiler is attached and passes untraced, the way the gate runs it.
        entry = textwrap.dedent('''
            import sys as _sys

            class HarnessSensitive(unittest.TestCase):
                def test_no_profiler(self):
                    self.assertIsNone(_sys.monitoring.get_tool(_sys.monitoring.PROFILER_ID))
        ''')
        traced, _, _ = self.trace_runner_fixture("harness_sensitive", entry, "discover")
        plain, _, _ = self.trace_runner_fixture("harness_sensitive", entry, "discover", plain=True)
        self.assertEqual(traced[-1]["failed"], ["HarnessSensitive.test_no_profiler"])
        self.assertEqual(plain[-1]["failed"], [])
        self.assertEqual([r for r in plain if not r.get("summary")], [])  # plain records no witness
        summary = pw.reconcile(traced[-1], plain[-1])
        self.assertEqual((summary["failed"], summary["harness_only"], summary["traced_failed"]),
                         ([], ["HarnessSensitive.test_no_profiler"],
                          ["HarnessSensitive.test_no_profiler"]))
        self.assertEqual(pw.registry_problems([], [summary], {}), [])

    def test_discovery_suites_take_the_plain_discover_verdict(self):
        # A discovery suite's gate verdict is the whole-directory `unittest discover`, whose
        # import order some suites depend on: only its failures for that module count.
        traced = [
            {"suite": "test_alone", "summary": True, "unittest": True, "cases": 2,
             "failed": ["A.order_dependent", "A.broken"], "error": None},
            {"suite": "test_clean", "summary": True, "unittest": True, "cases": 1,
             "failed": [], "error": None},
            {"suite": "loop_suite", "summary": True, "unittest": False, "cases": 1,
             "failed": [], "error": "ValueError: x"},
        ]
        verdict = {"test_alone": {"failed": ["A.broken"], "error": None}}
        merged = pw.reconcile_discovered(traced, {"test_alone", "test_clean"}, verdict)
        self.assertEqual((merged[0]["failed"], merged[0]["harness_only"]),
                         (["A.broken"], ["A.order_dependent"]))
        self.assertIs(merged[1], traced[1])
        self.assertIs(merged[2], traced[2])  # loop suites are rerun alone, not here
        self.assertEqual(pw.registry_problems([], merged, {}), [
            "test_alone: 1 test identities failed", "loop_suite: suite failed: ValueError: x"])

    def test_reconcile_keeps_the_gates_failures(self):
        traced = {"suite": "s", "summary": True, "unittest": True, "cases": 3,
                  "failed": ["C.a", "C.b"], "error": None}
        both = pw.reconcile(traced, {"failed": ["C.b"], "error": None})
        self.assertEqual((both["failed"], both["harness_only"]), (["C.b"], ["C.a"]))
        self.assertEqual(pw.registry_problems([], [both], {}), ["s: 1 test identities failed"])
        raised = dict(traced, failed=[], error="RuntimeIdentityError: wheel")
        clean = pw.reconcile(raised, {"failed": [], "error": None})
        self.assertEqual((clean["error"], clean["harness_error"]),
                         (None, "RuntimeIdentityError: wheel"))
        self.assertEqual(pw.registry_problems([], [clean], {}), [])
        still = pw.reconcile(raised, {"failed": [], "error": "ValueError: real"})
        self.assertEqual(pw.registry_problems([], [still], {}), ["s: suite failed: ValueError: real"])

    def test_explicit_text_runner_executes_once_while_its_pickle_module_is_live(self):
        records, calls, restored = self.trace_runner_fixture("text_runner", textwrap.dedent('''
            if __name__ == "__main__":
                unittest.TextTestRunner(verbosity=0).run(
                    unittest.defaultTestLoader.loadTestsFromTestCase(RunnerCase))
        '''))
        self.assertEqual(calls, ["pickle"])
        self.assertEqual(restored, (True, True, True))
        self.assertEqual((records[-1]["cases"], records[-1]["failed"], records[-1]["error"]),
                         (1, [], None))
        witnessed = [record for record in records if not record.get("summary")
                     and record["passed"]]
        self.assertEqual([record["test"] for record in witnessed], ["RunnerCase.test_pickle"])
        self.assertEqual(set(witnessed[0]["rows"]), {0, 2})

    def test_unittest_main_executes_once_and_restores_its_process_hooks(self):
        records, calls, restored = self.trace_runner_fixture("main_runner", textwrap.dedent('''
            if __name__ == "__main__":
                unittest.main()
        '''))
        self.assertEqual(calls, ["pickle"])
        self.assertEqual(restored, (True, True, True))
        self.assertEqual((records[-1]["cases"], records[-1]["failed"], records[-1]["error"]),
                         (1, [], None))

    def test_discovery_imports_its_pickle_module_and_executes_cases_once(self):
        records, calls, restored = self.trace_runner_fixture("discovered_runner", "", "discover")
        self.assertEqual(calls, ["pickle"])
        self.assertEqual(restored, (True, True, True))
        self.assertEqual((records[-1]["cases"], records[-1]["failed"], records[-1]["error"]),
                         (1, [], None))

    def test_a_direct_script_does_not_invent_a_missing_unittest_runner(self):
        records, calls, _ = self.trace_runner_fixture("no_runner", "")
        self.assertEqual(calls, [])
        self.assertEqual((records[-1]["unittest"], records[-1]["cases"]), (True, 0))
        self.assertIn("no_runner: defines tests but executed none",
                      pw.registry_problems([], records, {}))

    def test_main_failure_keeps_other_passing_identities_but_refuses_the_suite(self):
        records, calls, _ = self.trace_runner_fixture("failed_main", textwrap.dedent('''
            def failure(self):
                witness.runner_calls.append("failure")
                self.assertEqual(witness.Square().width(), -1)
            RunnerCase.test_bad = failure
            if __name__ == "__main__":
                unittest.main()
        '''))
        self.assertEqual(calls, ["failure", "pickle"])
        self.assertEqual((records[-1]["cases"], records[-1]["failed"], records[-1]["error"]),
                         (2, ["RunnerCase.test_bad"], None))
        passed = [record["test"] for record in records if record.get("passed")]
        self.assertEqual(passed, ["RunnerCase.test_pickle"])
        self.assertEqual(pw.registry_problems([], records, {}),
                         ["failed_main: 1 test identities failed"])

    def test_function_test_cases_keep_their_individual_identities(self):
        records, calls, _ = self.trace_runner_fixture("function_runner", textwrap.dedent('''
            def test_first():
                witness.runner_calls.append("first")
                assert witness.Square().width() == 2
            def test_second():
                witness.runner_calls.append("second")
                assert witness.Circle().width() == 2
            unittest.TextTestRunner(verbosity=0).run(unittest.TestSuite([
                unittest.FunctionTestCase(test_first), unittest.FunctionTestCase(test_second)]))
        '''))
        self.assertEqual(calls, ["first", "second"])
        self.assertEqual((records[-1]["cases"], records[-1]["failed"]), (2, []))
        passed = {record["test"]: set(record["rows"]) for record in records
                  if record.get("passed")}
        self.assertEqual(passed, {"test_first": {0, 2}, "test_second": {1}})

    def test_result_records_failures_subtest_failures_and_unexpected_successes(self):
        class Bad(unittest.TestCase):
            def test_failure(self):
                self.fail("planted failure")

            def test_subtest(self):
                with self.subTest("planted subtest"):
                    self.fail("planted subtest failure")

            @unittest.expectedFailure
            def test_unexpected_success(self):
                pass

        for name in ("test_failure", "test_subtest", "test_unexpected_success"):
            with self.subTest(name=name):
                recorder = pw.Recorder(pw.Probes([]), __file__, set())
                result = pw._Result(recorder)
                Bad(name).run(result)
                self.assertFalse(result.wasSuccessful())
                self.assertEqual(result.failed, {f"Bad.{name}"})

    def test_a_failing_or_raising_suite_cannot_pass_the_registry_check(self):
        records = [{"suite": "failed", "summary": True, "unittest": True, "cases": 2,
                    "failed": ["Case.test_bad"], "error": None},
                   {"suite": "raised", "summary": True, "unittest": False, "cases": 1,
                    "failed": [], "error": "RuntimeError: planted"}]
        self.assertEqual(pw.registry_problems([], records, {}), [
            "failed: 1 test identities failed", "raised: suite failed: RuntimeError: planted"])

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

    def test_a_staticmethod_member_counts_when_its_code_runs(self):
        # A staticmethod has no instance receiver (manimlib's StringMobject.span_contains, and
        # the decorators the portal defines as staticmethods, such as Mobject.affects_data), so
        # calling it means running its code, as for a module function. The class row still
        # needs an instance of its exact class.
        rows = [row("Span.contains", "method"), row("Span", "class")]
        probes = pw.Probes(rows)
        recorder = pw.Recorder(probes, __file__, render_rows=set())

        def test_span(mod=self.mod):
            mod.Span.contains((0, 3), (1, 2))
        with recorder:
            test_span()
        self.assertEqual({rows[i]["symbol"] for i in recorder.hits["test_span"]},
                         {"Span.contains"})
        # Its trivial mutant stays a staticmethod, so a witness test sees None, not a TypeError.
        original = pw.inspect.getattr_static(self.mod.Span, "contains")
        pw.mutate(probes, 0, "trivial")
        try:
            self.assertIsNone(self.mod.Span.contains((0, 3), (1, 2)))
        finally:
            self.mod.Span.contains = original

    def test_an_installer_renamed_member_counts_by_its_code(self):
        # The portal installs implementations whose code keeps its own name: Animation.copy
        # runs code named animation_copy, FadeTransform.clean_up_from_scene runs `cleanup`. The
        # row's code object answers for the member's name. One code object installed under two
        # names (a getter factory, a decorator's wrapper) is ambiguous and witnesses neither.
        rows = [row("Tile.area", "method"), row("Box.get_side", "method"),
                row("Box.get_depth", "method")]
        recorder = pw.Recorder(pw.Probes(rows), __file__, render_rows=set())

        def test_tile(mod=self.mod):
            mod.Tile().area()
            mod.Box().get_side()
        with recorder:
            test_tile()
        self.assertEqual({rows[i]["symbol"] for i in recorder.hits["test_tile"]}, {"Tile.area"})

    def test_a_cached_function_counts_when_its_wrapped_code_runs(self):
        # functools.lru_cache wraps a function in a C object with no code
        # (manimlib.mobject.numbers.char_to_cahced_mob): the wrapped function's code answers.
        rows = [row("cached_area", "function")]
        recorder = pw.Recorder(pw.Probes(rows), __file__, render_rows=set())

        # A run earlier in the same interpreter may have cached this call already.
        self.mod.cached_area.cache_clear()

        def test_cached(mod=self.mod):
            mod.cached_area(3.0)
        with recorder:
            test_cached()
        self.assertEqual({rows[i]["symbol"] for i in recorder.hits["test_cached"]},
                         {"cached_area"})

    def test_a_refusing_constructor_is_not_a_successful_class_witness(self):
        class Refusing:
            def __init__(self):
                self.helper()
                raise NotImplementedError("private asset")

            def helper(self):
                return None

        self.mod.Refusing = Refusing
        rows = [row("Refusing", "class"), row("Refusing.helper", "method")]
        recorder = pw.Recorder(pw.Probes(rows), __file__, render_rows=set())
        with recorder:
            with self.assertRaises(NotImplementedError):
                self.mod.Refusing()
        self.assertEqual(recorder.successes["<module>"], {1})
        self.assertEqual(recorder.refusals["<module>"], {0})

    def test_only_normal_completion_witnesses_a_python_or_native_call(self):
        self.mod.native_index = list.index

        def checked(value):
            if value < 0:
                raise ValueError("negative")
            try:
                raise LookupError("internally handled")
            except LookupError:
                return None

        self.mod.checked = checked
        rows = [row("checked", "function"), row("native_index", "function")]
        recorder = pw.Recorder(pw.Probes(rows), __file__, render_rows={1})

        def test_refusals(mod=self.mod):
            for function, args in ((mod.checked, (-1,)), (mod.native_index, ([], 3))):
                try:
                    function(*args)
                except ValueError:
                    pass

        def test_success(mod=self.mod):
            assert mod.checked(1) is None
            assert mod.native_index([3], 3) == 0

        with recorder:
            test_refusals()
            test_success()
        self.assertEqual(recorder.successes["test_refusals"], set())
        self.assertEqual(recorder.refusals["test_refusals"], {0, 1})
        self.assertEqual(recorder.successes["test_success"], {0, 1})
        self.assertEqual(recorder.refusals["test_success"], set())
        self.assertEqual(recorder.renders, {"test_success"})

    def test_refusal_only_same_or_improved_row_fails_without_a_committed_baseline(self):
        for status in ("same", "improved"):
            rows = [row("maybe_area", "function", status=status)]
            probes = pw.Probes(rows)
            recorder = pw.Recorder(probes, __file__, render_rows={0})

            def test_refusal(mod=self.mod):
                for _ in range(3):
                    try:
                        mod.maybe_area(-1)
                    except ValueError:
                        pass

            with recorder:
                test_refusal()
            records = [{"suite": "planted", "test": "test_refusal", "passed": True,
                        "rows": sorted(recorder.successes["test_refusal"]),
                        "refusals": sorted(recorder.refusals["test_refusal"]),
                        "renders": "test_refusal" in recorder.renders}]
            table = pw.registry(rows, probes, records)
            self.assertEqual(table[0]["semantic"], 0)
            self.assertEqual(table[0]["rendered"], 0)
            self.assertEqual(table[0]["refusal_only"], 1)
            self.assertEqual(pw.registry_problems(table, records, {}), [
                f"fmn_witness_synthetic.maybe_area: {status} in the ledger, but "
                "its 1 passing witnesses only exercise refusals"])
            failed = [dict(records[0], passed=False)]
            self.assertEqual(pw.registry(rows, probes, failed)[0]["refusal_only"], 0)

    def test_an_abstract_class_refusal_is_its_semantics_but_a_concrete_one_is_not(self):
        # An abstract class refuses construction in the Reference too (CoordinateSystem,
        # ShowPartial, StringMobject), so that refusal is no overclaim. A concrete class
        # whose constructor refuses still fails the check.
        rows = [row("Template", "class"), row("Sealed", "class")]
        probes = pw.Probes(rows)
        recorder = pw.Recorder(probes, __file__, render_rows=set())

        def test_refusals(mod=self.mod):
            for cls in (mod.Template, mod.Sealed):
                try:
                    cls()
                except (TypeError, NotImplementedError):
                    pass

        with recorder:
            test_refusals()
        records = [{"suite": "planted", "test": "test_refusals", "passed": True,
                    "rows": sorted(recorder.successes["test_refusals"]),
                    "refusals": sorted(recorder.refusals["test_refusals"]),
                    "renders": False}]
        table = pw.registry(rows, probes, records)
        self.assertEqual([(r["symbol"], r["semantic"], r["refusal_only"], r["abstract"])
                          for r in table], [("Template", 0, 1, True), ("Sealed", 0, 1, False)])
        self.assertEqual(pw.registry_problems(table, records, {}), [
            "fmn_witness_synthetic.Sealed: same in the ledger, but "
            "its 1 passing witnesses only exercise refusals"])

    def test_success_and_refusal_in_one_test_still_earn_a_successful_witness(self):
        rows = [row("maybe_area", "function")]
        probes = pw.Probes(rows)
        recorder = pw.Recorder(probes, __file__, render_rows=set())

        def test_mixed(mod=self.mod):
            try:
                mod.maybe_area(-1)
            except ValueError:
                pass
            assert mod.maybe_area(2) == 4

        with recorder:
            test_mixed()
        self.assertEqual(recorder.successes["test_mixed"], {0})
        self.assertEqual(recorder.refusals["test_mixed"], {0})
        records = [{"suite": "mixed", "test": "test_mixed", "passed": True,
                    "rows": [0], "refusals": [0], "renders": False}]
        table = pw.registry(rows, probes, records)
        self.assertEqual((table[0]["semantic"], table[0]["refusal_only"]), (1, 0))
        self.assertEqual(pw.registry_problems(table, records, {}), [])

    def test_v1_registry_columns_keep_their_original_meanings(self):
        prior = ("# fmn.portal-witnesses v1: generated baseline\n"
                 "module\tsymbol\tkind\tstatus\timport\tsemantic\trendered\tcorpus\texample\n"
                 "fmn_witness_synthetic\tmaybe_area\tfunction\tsame\tok\t2\t3\t4\ts::t\n")
        record = pw.read_registry(prior)[("fmn_witness_synthetic", "maybe_area")]
        self.assertEqual((record["semantic"], record["rendered"], record["corpus"]),
                         ("2", "3", "4"))
        self.assertNotIn("refusal_only", record)
        with self.assertRaises(ValueError):
            pw.read_registry(prior.replace("\ts::t", ""))
        with self.assertRaises(ValueError):
            pw.read_registry(prior.replace("v1:", "v999:"))

    def test_generator_yield_is_not_completion_and_abandonment_keeps_no_frame_alive(self):
        rows = [row("delayed_area", "function")]
        recorder = pw.Recorder(pw.Probes(rows), __file__, render_rows=set())

        def test_abandoned(mod=self.mod):
            generator = mod.delayed_area(-1)
            observer = weakref.ref(generator)
            assert next(generator) == -1
            del generator
            gc.collect()
            assert observer() is None, "the tracer retained an abandoned generator"

        def test_refused_generator(mod=self.mod):
            try:
                list(mod.delayed_area(-1))
            except ValueError:
                pass

        def test_completed_generator(mod=self.mod):
            assert list(mod.delayed_area(2)) == [2]

        with recorder:
            test_abandoned()
            test_refused_generator()
            test_completed_generator()
        self.assertEqual(recorder.successes["test_abandoned"], set())
        self.assertEqual(recorder.successes["test_refused_generator"], set())
        self.assertEqual(recorder.refusals["test_refused_generator"], {0})
        self.assertEqual(recorder.successes["test_completed_generator"], {0})

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
        tests = pathlib.Path(self.tmp.name)
        (tests / "test_first.py").write_text("")
        (tests / "test_last.py").write_text("")
        direct = "python3 crates/fmn-python/tests/test_first.py\n"
        discover = "python3 -m unittest discover -s crates/fmn-python/tests -p 'test_*.py'\n"
        self.assertEqual(pw.gate_suites(script + direct + discover, tests)[-2:],
                         [("test_first", "main"), ("test_last", "discover")])

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
