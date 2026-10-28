#!/usr/bin/env python3
"""Tests for scripts/tex_grammar_differential.py (fm-tex-grammar-differential-r12-i0nh):
the harness, not the boxes it records.

They run anywhere, without LaTeX or cargo:
- the generator is deterministic per seed and its formulas balance their braces;
- every sub-formula is a shrink candidate of its root;
- the batch log parser takes each box by its own marker and drops a formula whose
  segment logs an error;
- the report shrinks a mismatch to its shortest mismatching candidate;
- the report counts a plant's detections, and fails when a plant moves no case.
"""
from __future__ import annotations

import argparse
import contextlib
import io
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import tex_grammar_differential as differential  # noqa: E402


def balanced(text: str) -> bool:
    depth = 0
    escaped = False
    for char in text:
        if escaped:
            escaped = False
        elif char == "\\":
            escaped = True
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth < 0:
                return False
    return depth == 0


class Generator(unittest.TestCase):
    def test_a_seed_always_generates_the_same_formulas(self):
        first = [node.tex for node in differential.formulas(7, 50)]
        self.assertEqual(first, [node.tex for node in differential.formulas(7, 50)])
        self.assertNotEqual(first, [node.tex for node in differential.formulas(8, 50)])

    def test_formulas_balance_their_braces(self):
        for seed in range(1, 4):
            for root in differential.formulas(seed, 200):
                for sub in differential.subformulas(root):
                    self.assertTrue(balanced(sub.tex), sub.tex)

    def test_every_subformula_is_a_candidate_of_its_root(self):
        with tempfile.TemporaryDirectory() as work:
            run = pathlib.Path(work)
            args = argparse.Namespace(seeds="3", count=25, out=str(run))
            with contextlib.redirect_stdout(io.StringIO()):
                differential.generate(args)
            cases = dict(line.split("\t", 1) for line in
                         (run / "cases.tsv").read_text(encoding="utf-8").splitlines())
            roots = json.loads((run / "roots.json").read_text(encoding="utf-8"))["roots"]
            self.assertEqual(len(roots), 25)
            for root, node in zip(roots, differential.formulas(3, 25)):
                keys = {key for key, _ in root["candidates"]}
                self.assertIn(root["id"], keys)
                for sub in differential.subformulas(node):
                    self.assertEqual(cases[differential.case_id(sub.tex)], sub.tex)
                    self.assertIn(differential.case_id(sub.tex), keys)


class BatchLog(unittest.TestCase):
    def test_each_box_is_read_by_its_marker_and_errors_drop_the_formula(self):
        log = ("FMNBEGIN aaaa\nFMNBOX aaaa 5.0pt 6.44444pt 0.0pt\n"
               "FMNBEGIN bbbb\n! Undefined control sequence.\nFMNBOX bbbb 1.0pt 1.0pt 1.0pt\n"
               "FMNBEGIN cccc\nFMNBOX cccc 10.0pt 7.5pt 2.5pt\n")

        def fake_latex(argv, cwd, **kwargs):
            del argv, kwargs
            (pathlib.Path(cwd) / "f.log").write_text(log, encoding="latin-1")
            return subprocess.CompletedProcess([], 1)

        with mock.patch.object(differential.subprocess, "run", fake_latex):
            boxes = differential.tex_batch([("aaaa", "x"), ("bbbb", "\\nope"), ("cccc", "y")],
                                           "", False, "")
        self.assertEqual(boxes["aaaa"], (0.5, 0.644444, 0.0))
        self.assertIsNone(boxes["bbbb"])
        self.assertEqual(boxes["cccc"], (1.0, 0.75, 0.25))


class Report(unittest.TestCase):
    def write_run(self, run: pathlib.Path, planted: dict[str, str] | None = None) -> None:
        cases = {"root": "a + \\frac{b}{c}", "frac": "\\frac{b}{c}", "a": "a", "b": "b"}
        (run / "cases.tsv").write_text("".join(f"{k}\t{v}\n" for k, v in cases.items()),
                                       encoding="utf-8")
        roots = {"seeds": "1", "count": 1, "roots": [{
            "seed": 1, "index": 0, "id": "root", "kind": "seq",
            "candidates": [["root", "seq"], ["a", "letter"], ["frac", "frac"], ["b", "letter"]],
        }]}
        (run / "roots.json").write_text(json.dumps(roots), encoding="utf-8")
        tex = {"root": "2.0\t1.0\t0.5", "frac": "1.0\t1.0\t0.5", "a": "0.5\t0.4\t0.0",
               "b": "0.4\t0.7\t0.0"}
        # fmd-math's fraction is wide; the root mismatches through it alone.
        fmd = {"root": "2.01\t1.0\t0.5", "frac": "1.01\t1.0\t0.5", "a": "0.5\t0.4\t0.0",
               "b": "0.4\t0.7\t0.0"}
        (run / "tex_boxes.tsv").write_text("".join(f"{k}\t{v}\n" for k, v in tex.items()),
                                           encoding="utf-8")
        (run / "fmd_boxes.tsv").write_text("".join(f"{k}\t{v}\n" for k, v in fmd.items()),
                                           encoding="utf-8")
        if planted is not None:
            (run / "tex_boxes.sup1.tsv").write_text(
                "".join(f"{k}\t{planted.get(k, v)}\n" for k, v in tex.items()), encoding="utf-8")

    def run_report(self, run: pathlib.Path, plant: str | None) -> tuple[int, dict, list[dict]]:
        out = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
            code = differential.report(argparse.Namespace(run=str(run), plant=plant))
        summary = json.loads(out.getvalue().splitlines()[0])
        name = "report" + (f".{plant}" if plant else "") + ".ndjson"
        records = [json.loads(line) for line in
                   (run / name).read_text(encoding="utf-8").splitlines()]
        return code, summary, records

    def test_a_mismatch_shrinks_to_its_shortest_mismatching_candidate(self):
        with tempfile.TemporaryDirectory() as work:
            run = pathlib.Path(work)
            self.write_run(run)
            code, summary, records = self.run_report(run, None)
            self.assertEqual(code, 0)
            self.assertEqual(summary["mismatch"], 1)
            self.assertEqual(records[0]["minimal"], "\\frac{b}{c}")
            self.assertEqual(records[0]["dims"], "w")

    def test_a_plant_counts_the_cases_it_moves_and_fails_when_it_moves_none(self):
        with tempfile.TemporaryDirectory() as work:
            run = pathlib.Path(work)
            # The planted oracle raises b's height, so b mismatches only under it.
            self.write_run(run, planted={"b": "0.4\t0.71\t0.0"})
            code, summary, _ = self.run_report(run, "sup1")
            self.assertEqual(code, 0)
            self.assertEqual(summary["plant_detected_cases"], 1)
        with tempfile.TemporaryDirectory() as work:
            run = pathlib.Path(work)
            self.write_run(run, planted={})
            code, summary, _ = self.run_report(run, "sup1")
            self.assertEqual(code, 1)
            self.assertEqual(summary["plant_detected_cases"], 0)


if __name__ == "__main__":
    unittest.main()
