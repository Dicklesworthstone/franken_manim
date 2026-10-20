#!/usr/bin/env python3
"""Tests for scripts/corpus_differential.py (fm-5wq.33): the harness, not the facts it records.

The unit tests run anywhere: the outcome classifier (timeouts and crashes included), the
one-engine triage table, ddmin on its own and through `minimize` with stand-in engines
whose planted difference is known, and the dashboard, which must depend only on the set
of records and never carry frame paths or error text (the corpus is CC BY-NC-SA).

parity.corpus_differential.v1 runs the three checked-in self-test scenes
(crates/fmn-conformance/fixtures/corpus_differential/selftest_scenes.py) through the
pinned Reference and the installed portal. It needs FMN_REFERENCE_PYTHON and
FMN_PORTAL_PYTHON (and the Reference checkout, xvfb-run and TeX); without them it records
`SKIPPED parity.corpus_differential.v1 reference-env-absent: ...` and fails under
FMN_REQUIRE_FULL_INPUTS=1 (fm-5wq.48), so a missing Reference never reads as a pass.
"""
from __future__ import annotations

import ast
import json
import os
import pathlib
import random
import shutil
import subprocess
import sys
import tempfile
import types
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import corpus_differential as cd  # noqa: E402
import structural_facts as sf  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[1]
SELFTEST = ROOT / "crates" / "fmn-conformance" / "fixtures" / "corpus_differential" / "selftest_scenes.py"
EXCLUSIONS = sf.in_scope(sf.load_exclusions(cd.EXCLUSIONS), "scenes")


def skip_or_fail(test: unittest.TestCase, name: str, missing: str) -> None:
    """A required input is absent (fm-5wq.48): fail under
    FMN_REQUIRE_FULL_INPUTS=1, otherwise record a SKIPPED ledger line and skip."""
    if os.environ.get("FMN_REQUIRE_FULL_INPUTS") == "1":
        test.fail(f"{name}: required input missing: {missing} (FMN_REQUIRE_FULL_INPUTS=1)")
    line = f"SKIPPED {name} {missing}\n"
    sys.stderr.write(line)
    ledger = os.environ.get("FMN_SKIP_LEDGER")
    if ledger:
        with open(ledger, "a", encoding="utf-8") as handle:
            handle.write(line)
    test.skipTest(f"{missing} is absent")


def member(path, cls, n_points=0):
    return {"path": path, "class": cls, "mro": [cls], "n_points": n_points}


def facts(scene, members, **extra):
    return {"schema": sf.SCHEMA, "version": sf.VERSION, "subject": scene, "members": members, **extra}


class ClassifierTests(unittest.TestCase):
    def test_an_engine_ran_only_on_exit_zero_with_error_free_facts(self):
        clean = facts("S", [member("0", "Circle")])
        self.assertTrue(cd.ran_scene(0, clean))
        self.assertFalse(cd.ran_scene(None, clean), "a timeout has no exit code")
        self.assertFalse(cd.ran_scene(3, clean), "facts written before a later crash")
        self.assertFalse(cd.ran_scene(0, None), "no facts at all")
        self.assertFalse(cd.ran_scene(0, {"subject": "S", "error": "TypeError: bad"}))

    def test_outcome_codes(self):
        self.assertEqual(cd.outcome(True, True), "both")
        self.assertEqual(cd.outcome(True, False), "portal-only")
        self.assertEqual(cd.outcome(False, True), "reference-only")
        self.assertEqual(cd.outcome(False, False), "neither")

    def run_with(self, portal, reference):
        """run_one with stand-in engines: each side is (exit, stderr, facts or None)."""
        out = pathlib.Path(tempfile.mkdtemp(prefix="fmn-cd-"))
        self.addCleanup(shutil.rmtree, out, True)
        args = types.SimpleNamespace(out=out, exclusions=EXCLUSIONS, engine_ids="reference=R portal=P")

        def engine(side, spec):
            def run(args, module, scene, work):
                code, stderr, record = spec
                if record is not None:
                    (work / f"{side}.ndjson").write_text(sf.canonical(record) + "\n")
                return (code, stderr, None), work / f"{side}.png"
            return run

        saved = cd.run_portal, cd.run_reference
        cd.run_portal, cd.run_reference = engine("portal", portal), engine("reference", reference)
        try:
            return cd.run_one(args, "_2021/m.py", "S")
        finally:
            cd.run_portal, cd.run_reference = saved

    def test_run_one_classifies_timeouts_and_crashes(self):
        ok = (0, "", facts("S", [member("0", "Circle")]))
        timeout = (None, None, None)
        crash = (1, "Traceback ...\nTypeError: unsupported operand\n", facts("S", [member("0", "Circle")]))
        record = self.run_with(ok, timeout)
        self.assertEqual(record["outcome"], "portal-only")
        self.assertEqual(record["triage"]["label"], "timeout")
        self.assertEqual(record["triage"]["side"], "reference")
        record = self.run_with(crash, ok)
        self.assertEqual(record["outcome"], "reference-only")
        self.assertEqual(record["portal"]["error"], "TypeError: unsupported operand")
        self.assertEqual(record["triage"]["label"], "untriaged")
        record = self.run_with(timeout, timeout)
        self.assertEqual(record["outcome"], "neither")
        self.assertEqual(record["triage"]["label"], "timeout")

    def test_run_one_diffs_two_engine_scenes_in_both_tiers(self):
        record = self.run_with((0, "", facts("S", [member("0", "Square", 4)])),
                               (0, "", facts("S", [member("0", "Circle", 4)])))
        self.assertEqual(record["outcome"], "both")
        for tier in ("structure", "geometry"):
            self.assertEqual(record[tier]["verdict"], "differs")
            self.assertEqual(record[tier]["first_difference"]["fact"], "class")
            self.assertEqual(record[tier]["first_difference"]["reference"], "Circle")
        self.assertEqual(record["engines"], "reference=R portal=P")
        self.assertIsNotNone(record["portal"]["facts_sha"])
        self.assertIsNotNone(record["reference"]["facts_sha"])


class TriageTests(unittest.TestCase):
    @staticmethod
    def record(portal_error=None, reference_error=None, portal_exit=1, reference_exit=1):
        return {"portal": {"exit": portal_exit, "error": portal_error},
                "reference": {"exit": reference_exit, "error": reference_error}}

    def test_a_reference_side_rule_never_matches_the_portal(self):
        error = "Exception: Must specify either a file_name or svg_string SVGMobject"
        self.assertEqual(cd.triage(self.record(reference_error=error))["label"], "reference-defect")
        self.assertEqual(cd.triage(self.record(portal_error=error))["label"], "untriaged")

    def test_an_any_side_rule_names_the_side_it_matched(self):
        result = cd.triage(self.record(portal_error="ModuleNotFoundError: No module named 'gensim'"))
        self.assertEqual((result["label"], result["side"]), ("environment", "portal"))

    def test_the_first_matching_rule_wins(self):
        # Matches both the environment rule (missing module) and the leniency rule's
        # keyword pattern; the environment rule is earlier in the table.
        error = "ModuleNotFoundError: unexpected keyword argument"
        self.assertEqual(cd.triage(self.record(reference_error=error))["label"], "environment")

    def test_the_same_error_in_both_engines_is_agreement(self):
        portal = ("fmn-python: scene/scene-execution-failed: TypeError: Object <bound method "
                  "Mobject.move_to of <Point object at 0x78d7d6b9c370>> cannot be converted to an animation")
        reference = ("TypeError: Object <bound method Mobject.move_to of <Point object at 0x7f2a3f19eba0>> "
                     "cannot be converted to an animation")
        result = cd.triage(self.record(portal_error=portal, reference_error=reference))
        self.assertEqual((result["label"], result["side"]), ("agree-error", "both"))
        # A cut-off line still agrees with its full counterpart.
        self.assertEqual(cd.triage(self.record(portal_error=portal[:120], reference_error=reference))["label"],
                         "agree-error")

    def test_the_same_exception_class_with_another_message_is_not_agreement(self):
        result = cd.triage(self.record(portal_error="TypeError: missing a required argument: 'values'",
                                       reference_error="TypeError: BarChart.__init__() missing 1 required "
                                                       "positional argument: 'values'"))
        self.assertEqual(result, {"label": "untriaged"})

    def test_a_portal_policy_never_labels_a_scene_the_reference_cannot_run(self):
        refusal = ("fmn-python: scene/scene-execution-failed: ValueError: font family 'Consolas' is not "
                   "available; bundled families: Computer Modern, CM Typewriter, IBM Plex Sans")
        ran = cd.triage(self.record(portal_error=refusal, reference_exit=0))
        self.assertEqual((ran["label"], ran["side"]), ("owner-ruling", "portal"))
        failed = cd.triage(self.record(portal_error=refusal, reference_error="IndexError: index 5 is out of bounds"))
        self.assertEqual(failed, {"label": "untriaged"})

    def test_a_bare_assert_is_named_by_its_source_line(self):
        stderr = ('  File "vectorized_mobject.py", line 802, in get_nth_curve_points\n'
                  "    assert n < self.get_num_curves()\n           ^^^^^^^^^^^^^^^^^^^^^^^^^\nAssertionError\n")
        error = cd._last_error(stderr)
        self.assertEqual(error, "AssertionError (assert n < self.get_num_curves())")
        result = cd.triage(self.record(reference_error=error, portal_exit=0))
        self.assertEqual((result["label"], result["side"]), ("reference-defect", "reference"))
        self.assertEqual(cd._last_error("Traceback\nAssertionError: explicit message\n"),
                         "AssertionError: explicit message")

    def test_timeout_is_the_fallback_and_untriaged_is_explicit(self):
        result = cd.triage(self.record(portal_exit=None))
        self.assertEqual((result["label"], result["side"]), ("timeout", "portal"))
        self.assertEqual(cd.triage(self.record(portal_error="ValueError: odd", reference_exit=0)),
                         {"label": "untriaged"})


class DdminTests(unittest.TestCase):
    def test_one_cause_reduces_to_exactly_that_statement(self):
        for count in (1, 2, 3, 13, 40):
            for cause in {0, count // 2, count - 1}:
                calls = []

                def reproduces(kept, cause=cause):
                    calls.append(kept)
                    return cause in kept

                self.assertEqual(cd.ddmin(count, reproduces), [cause])

    def test_an_interaction_reduces_to_exactly_the_pair(self):
        self.assertEqual(cd.ddmin(12, lambda kept: {2, 9} <= set(kept)), [2, 9])

    def test_results_are_one_minimal_for_random_cause_sets(self):
        rng = random.Random(533)
        for _ in range(200):
            count = rng.randint(1, 30)
            causes = set(rng.sample(range(count), rng.randint(1, min(4, count))))
            kept = cd.ddmin(count, lambda kept, causes=causes: causes <= set(kept))
            self.assertEqual(kept, sorted(causes))

    def test_an_exhausted_budget_returns_a_still_reproducing_superset(self):
        calls = {"n": 0}

        def reproduces(kept):
            calls["n"] += 1
            return 7 in kept

        kept = cd.ddmin(32, reproduces, exhausted=lambda: calls["n"] >= 2)
        self.assertEqual(calls["n"], 2)
        self.assertIn(7, kept)
        self.assertGreater(len(kept), 1)


PLANTED = '''from manimlib import *


class Planted(Scene):
    def construct(self):
        self.add(Circle())
        x = 1
        self.add(Square())
        self.add(VGroup())
        self.add(Dot())
        self.wait()
        self.add(Line())
        self.add(Square())
'''


class MinimizeTests(unittest.TestCase):
    """`minimize` over stand-in engines that read the variant module itself."""

    def setUp(self):
        self.root = pathlib.Path(tempfile.mkdtemp(prefix="fmn-cd-min-"))
        self.addCleanup(shutil.rmtree, self.root, True)
        self.videos = self.root / "videos"
        self.videos.mkdir()
        (self.videos / "planted.py").write_text(PLANTED, encoding="utf-8")
        self.args = types.SimpleNamespace(videos=self.videos, out=self.root / "out", exclusions=EXCLUSIONS)

    def minimize(self, rule, **kwargs):
        def engine(side):
            def run(args, module, scene, work):
                path = pathlib.Path(module)
                tree = ast.parse((path if path.is_absolute() else args.videos / path).read_text())
                construct = cd._construct_of(tree, scene)
                added = [s.value.args[0].func.id for s in construct.body
                         if isinstance(s, ast.Expr) and getattr(s.value.func, "attr", None) == "add"]
                members = [member(str(i), *rule(side, name, added[:i])) for i, name in enumerate(added)]
                (work / f"{side}.ndjson").write_text(sf.canonical(facts(scene, members)) + "\n")
                return (0, "", None), work / f"{side}.png"
            return run

        saved = cd.run_portal, cd.run_reference
        cd.run_portal, cd.run_reference = engine("portal"), engine("reference")
        try:
            return cd.minimize(self.args, "planted.py", "Planted", **kwargs)
        finally:
            cd.run_portal, cd.run_reference = saved

    def body(self, result):
        tree = ast.parse(pathlib.Path(result["minimized"]).read_text())
        return [ast.unparse(s) for s in cd._construct_of(tree, "Planted").body]

    def test_a_planted_class_difference_reduces_to_its_statement(self):
        # The planted engine bug: the portal reports every VGroup as a Group.
        result = self.minimize(lambda side, name, before: (
            "Group" if side == "portal" and name == "VGroup" else name, 0))
        self.assertEqual(result["target"], ["VGroup", "class"])
        self.assertEqual(self.body(result), ["self.add(VGroup())"])
        self.assertEqual((result["statements_before"], result["statements_after"]), (8, 1))
        self.assertFalse(result["budget_exhausted"])
        self.assertEqual((self.videos / "planted.py").read_text(), PLANTED, "the corpus source was modified")
        self.assertTrue(pathlib.Path(result["minimized"]).is_relative_to(self.args.out))

    def test_an_order_dependent_difference_keeps_both_statements(self):
        # The portal miscounts a Dot's points only when a Square was added before it.
        result = self.minimize(lambda side, name, before: (
            name, 1 if side == "portal" and name == "Dot" and "Square" in before else 0))
        self.assertEqual(result["target"], ["Dot", "n_points"])
        self.assertEqual(self.body(result), ["self.add(Square())", "self.add(Dot())"])

    def test_the_budget_bounds_the_engine_runs(self):
        result = self.minimize(lambda side, name, before: (
            "Group" if side == "portal" and name == "VGroup" else name, 0), budget=3)
        self.assertEqual(result["tests_run"], 3)
        self.assertTrue(result["budget_exhausted"])
        self.assertIn("self.add(VGroup())", self.body(result))

    def test_an_equal_scene_is_refused_not_minimized(self):
        result = self.minimize(lambda side, name, before: (name, 0))
        self.assertEqual(result["error"], "the full scene does not differ structurally")


def both(scene, verdict="equal", first=None, violations=()):
    tier = {"verdict": verdict, "first_difference": first, "difference_count": int(first is not None),
            "excluded": {}}
    return {"module": "_2021/demo.py", "scene": scene, "era": "2021", "outcome": "both",
            "engines": "reference=R portal=P",
            "portal": {"exit": 0, "error": None}, "reference": {"exit": 0, "error": None},
            "frames": {"reference": "/private/frames/ref.png", "portal": "/private/frames/portal.png"},
            "structure": dict(tier), "geometry": {**tier, "envelope_violations": list(violations)}}


def one_engine(scene, code, label, reason):
    return {"module": "_2022/other.py", "scene": scene, "era": "2022", "outcome": code,
            "engines": "reference=R portal=P",
            "portal": {"exit": 1, "error": "PRIVATE scene source line 12"},
            "reference": {"exit": 1, "error": "PRIVATE scene source line 12"},
            "frames": {"reference": None, "portal": "/private/frames/portal.png"},
            "triage": {"label": label, "reason": reason}}


def violation(cls, row, size_rel, cascade=False):
    return {"class": cls, "row": row, "family": [1, 1], "size_rel": size_rel,
            "envelope": {"size_rel": 0.1}, "cascade": cascade, "center_offset": 0.25}


class DashboardTests(unittest.TestCase):
    RECORDS = [
        both("Equal"),
        both("Excluded", "equal-with-exclusions"),
        both("Circled", "differs", {"class": "Circle", "fact": "class", "path": "0"}),
        both("Arrowed", "differs", {"class": "Arrow", "fact": "n_points", "path": "1"}),
        both("Arrowed2", "differs", {"class": "Arrow", "fact": "n_points", "path": "0"},
             [violation("Tex", "bn05-text-geometry", 0.5), violation("Tex", "bn05-text-geometry", 0.05, True)]),
        one_engine("Lenient", "portal-only", "portal-leniency", "a keyword"),
        one_engine("Slow", "reference-only", "timeout", "too slow"),
        one_engine("Absent", "neither", "environment", "a module"),
    ]

    def render(self, records):
        path = pathlib.Path(tempfile.mkdtemp(prefix="fmn-cd-dash-")) / "dashboard.md"
        self.addCleanup(shutil.rmtree, path.parent, True)
        cd.write_dashboard(records, path, "Test dashboard")
        return path.read_bytes()

    def test_the_page_depends_only_on_the_set_of_records(self):
        expected = self.render(self.RECORDS)
        rng = random.Random(33)
        for _ in range(8):
            shuffled = list(self.RECORDS)
            rng.shuffle(shuffled)
            self.assertEqual(self.render(shuffled), expected)

    def test_counts_and_tie_order(self):
        text = self.render(self.RECORDS).decode()
        for line in ("| candidates | 8 |", "| ran in both engines | 5 |", "| structure: equal | 1 |",
                     "| structure: equal-with-exclusions | 1 |", "| structure: differs | 3 |",
                     "| geometry: violates an envelope | 1 |", "| portal-only | 1 |",
                     "| reference-only | 1 |", "| neither | 1 |"):
            self.assertIn(line, text)
        # Equal counts sort by key: the Arrow cluster (2 scenes) first, then Circle;
        # one-engine labels tie at 1 and sort by outcome code.
        self.assertLess(text.index("| Arrow | n_points | 2 | `_2021/demo.py:Arrowed` |"),
                        text.index("| Circle | class | 1 |"))
        self.assertLess(text.index("| neither | environment |"), text.index("| portal-only | portal-leniency |"))
        self.assertIn("| Tex | bn05-text-geometry | 1 | 2 | 1 | 1 | 0 | 2 | 0.500 | 0.250 |", text)

    def test_every_differing_scene_carries_its_triage_or_says_it_has_none(self):
        triage = {
            "_2021/demo.py:Circled": {"first": "0 Circle.class", "label": "bug", "ref": "fm-x1", "note": ""},
            "_2021/demo.py:Arrowed": {"first": "9 Arrow.class", "label": "bug", "ref": "fm-x2", "note": ""},
            "_2021/demo.py:Equal": {"first": "0 Square.class", "label": "bug", "ref": "fm-x3", "note": ""},
        }
        path = pathlib.Path(tempfile.mkdtemp(prefix="fmn-cd-tri-")) / "dashboard.md"
        self.addCleanup(shutil.rmtree, path.parent, True)
        cd.write_dashboard(self.RECORDS, path, "Test dashboard", triage)
        text = path.read_text()
        self.assertIn("| `_2021/demo.py:Circled` | `0 Circle.class` | bug | fm-x1 |", text)
        # Its recorded first difference is no longer the scene's: stale, never kept.
        self.assertIn("| `_2021/demo.py:Arrowed` | `1 Arrow.n_points` | stale (was `9 Arrow.class`) | fm-x2 |", text)
        self.assertIn("| `_2021/demo.py:Arrowed2` | `0 Arrow.n_points` | untriaged |  |", text)
        self.assertIn("| bug | 1 |", text)
        self.assertIn("| stale | 1 |", text)
        self.assertIn("| untriaged | 1 |", text)
        self.assertIn("no longer differs (remove them): `_2021/demo.py:Equal`.", text)

    def test_the_registry_refuses_malformed_or_duplicate_rows(self):
        path = pathlib.Path(tempfile.mkdtemp(prefix="fmn-cd-reg-")) / "triage.tsv"
        self.addCleanup(shutil.rmtree, path.parent, True)
        row = "_2021/demo.py:Circled\t0 Circle.class\tbug\tfm-x1\tnote"
        path.write_text(f"# header\n{row}\n")
        self.assertEqual(cd.load_scene_triage(path)["_2021/demo.py:Circled"]["ref"], "fm-x1")
        for bad in (f"{row}\n{row}\n", "_2021/demo.py:Circled\tbug\n"):
            path.write_text(bad)
            with self.assertRaises(ValueError):
                cd.load_scene_triage(path)

    def test_no_frame_path_or_error_text_reaches_the_page(self):
        text = self.render(self.RECORDS).decode()
        self.assertNotIn("/private/frames", text)
        self.assertNotIn("PRIVATE", text)


class CorpusDifferentialEndToEnd(unittest.TestCase):
    """parity.corpus_differential.v1: three checked-in scenes through both engines."""

    NAME = "parity.corpus_differential.v1"

    def inputs(self):
        portal, reference = os.environ.get("FMN_PORTAL_PYTHON"), os.environ.get("FMN_REFERENCE_PYTHON")
        reference_root = pathlib.Path(os.environ.get("FMN_REFERENCE_ROOT", ROOT / "scripts" / "manim_ref"))
        missing = [what for what, present in (
            ("FMN_PORTAL_PYTHON", portal and os.access(portal, os.X_OK)),
            ("FMN_REFERENCE_PYTHON", reference and os.access(reference, os.X_OK)),
            (f"the Reference checkout {reference_root}", (reference_root / "manimlib").is_dir()),
            ("xvfb-run", shutil.which("xvfb-run")), ("pdftex", shutil.which("pdftex")),
            ("dvisvgm", shutil.which("dvisvgm"))) if not present]
        if missing:
            skip_or_fail(self, self.NAME, "reference-env-absent: " + ", ".join(missing))
        return portal, reference, reference_root

    def test_selftest_scenes_take_their_known_verdicts(self):
        portal, reference, reference_root = self.inputs()
        root = pathlib.Path(tempfile.mkdtemp(prefix="fmn-cd-e2e-"))
        self.addCleanup(shutil.rmtree, root, True)
        videos = root / "videos"
        videos.mkdir()
        shutil.copy(SELFTEST, videos / SELFTEST.name)
        scenes = ("EqualShapes", "NotedDotColor", "PlantedDifference")
        argv = [sys.executable, str(ROOT / "scripts" / "corpus_differential.py"),
                "--portal-python", portal, "--reference-python", reference,
                "--reference-root", str(reference_root), "--videos", str(videos), "--out", str(root / "out"),
                "--jobs", "3", "--timeout", "180", "--portal-id", "selftest"]
        for scene in scenes:
            argv += ["--scene", f"{SELFTEST.name}:{scene}"]
        run = subprocess.run(argv, capture_output=True, text=True, timeout=900, check=False)  # ubs:ignore — argv only, no shell; the interpreters are the operator's FMN_*_PYTHON.
        self.assertEqual(run.returncode, 0, run.stderr[-2000:])
        records = {r["scene"]: r for r in sf.read_ndjson(root / "out" / "records.ndjson")}
        self.assertEqual(sorted(records), sorted(scenes))
        for scene, record in sorted(records.items()):
            # One deterministic log line per scene: engines, fact digests, verdict, first difference.
            first = record.get("structure", {}).get("first_difference")
            print(json.dumps({"scenario": self.NAME, "scene": scene, "outcome": record["outcome"],
                              "engines": record["engines"],
                              "facts_sha": [record["portal"]["facts_sha"], record["reference"]["facts_sha"]],
                              "structure": record.get("structure", {}).get("verdict"),
                              "first_difference": first and f"{first['path']}:{first['class']}.{first['fact']}",
                              "excluded": record.get("structure", {}).get("excluded")}, sort_keys=True),
                  file=sys.stderr)
            self.assertEqual(record["outcome"], "both", record)
            self.assertIn("reference=3b1b/manim@6199a00d", record["engines"])
            self.assertIn("portal=selftest", record["engines"])
        self.assertEqual(records["EqualShapes"]["structure"]["verdict"], "equal")
        noted = records["NotedDotColor"]["structure"]
        self.assertEqual(noted["verdict"], "equal-with-exclusions")
        self.assertEqual(sorted(noted["excluded"]), ["bn07-c19-dot-color-keyword"])
        planted = records["PlantedDifference"]["structure"]
        self.assertEqual(planted["verdict"], "differs")
        self.assertEqual((planted["first_difference"]["reference"], planted["first_difference"]["portal"]),
                         ("Circle", "Square"))


if __name__ == "__main__":
    unittest.main()
