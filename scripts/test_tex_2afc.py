#!/usr/bin/env python3
"""Tests for the blind 2AFC TeX review kit and its scorer (fm-tex-2afc-kit-b9k2).

Pure Python over synthetic corpora, keys and images: the stratified draw, side assignment,
cropping, provenance parsing, the simulated reviewers, and the scorer's pass rule, including
its exact binomial operating characteristics. Building a real kit needs the pinned Reference
and an installed portal; that is run by hand (see tex_2afc_kit.py) and recorded on the bead.
"""
import base64
import contextlib
import io
import json
import math
import pathlib
import random
import shutil
import sys
import tempfile
import unittest

import numpy as np
from PIL import Image

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import score_tex_2afc as score  # noqa: E402
import tex_2afc_kit as kit  # noqa: E402


def binom_cdf(k, n, p):
    return sum(math.comb(n, i) * p**i * (1 - p) ** (n - i) for i in range(k + 1))


def corpus(rows):
    path = pathlib.Path(tempfile.mkdtemp(prefix="fmn-2afc-")) / "corpus.jsonl"
    lines = []
    for i, (text, constructs, count, *mode) in enumerate(rows):
        lines.append(json.dumps({"text": text, "constructs": constructs, "count": count,
                                 "mode": mode[0] if mode else "math", "sha256": f"{i:064x}", "sources": []}))
    path.write_text("\n".join(lines) + "\n")
    return path


def synthetic_key(n=100, strata=5):
    names = [f"s{j}" for j in range(strata)]
    return {"kit": "k1", "pairs": [{"pair": f"p{i:03d}", "latex_side": "left" if i % 2 else "right",
                                    "formula": f"{i:064x}", "stratum": names[i % strata]} for i in range(n)]}


def answers_for(key, correct_pairs, **extra):
    out = {}
    for p in key["pairs"]:
        right = p["pair"] in correct_pairs
        other = "left" if p["latex_side"] == "right" else "right"
        out[p["pair"]] = {"choice": p["latex_side"] if right else other, "visibly_wrong": False, "note": ""}
        out[p["pair"]].update(extra.get(p["pair"], {}))
    return {"kit": key["kit"], "answers": out}


class ScorerTests(unittest.TestCase):
    def test_the_wilson_interval_matches_known_values(self):
        lower, upper = score.wilson_interval(50, 100)
        self.assertAlmostEqual(lower, 0.4038, places=4)
        self.assertAlmostEqual(upper, 0.5962, places=4)
        self.assertEqual(score.wilson_interval(0, 10)[0], 0.0)

    def test_the_pass_limit_is_58_of_100_with_the_audited_operating_characteristics(self):
        self.assertEqual(score.pass_limit(100), 58)
        self.assertLessEqual(score.wilson_interval(58, 100)[1], 0.68)
        self.assertGreater(score.wilson_interval(59, 100)[1], 0.68)
        # The 2026-10-17 audit's four-place values (the bead's table rounds them to three).
        for p, expected in ((0.50, 0.9557), (0.60, 0.3775), (0.70, 0.0072)):
            self.assertAlmostEqual(binom_cdf(58, 100, p), expected, places=4)
        # The stratum rule's false-flag rate for an indistinguishable engine (the bead's 2.9%).
        tail = 1 - binom_cdf(15, 20, 0.5)
        self.assertAlmostEqual(1 - (1 - tail) ** 5, 0.029, places=3)

    def test_58_correct_passes_and_59_fails(self):
        key = synthetic_key()
        spread = [p["pair"] for p in key["pairs"]]
        self.assertEqual(score.score(answers_for(key, set(spread[:58])), key)["verdict"], "pass")
        report = score.score(answers_for(key, set(spread[:59])), key)
        self.assertEqual(report["verdict"], "fail")
        self.assertIn("59/100 identified exceeds 58", report["reasons"][0])

    def test_one_stratum_at_16_of_20_fails_even_below_the_overall_limit(self):
        key = synthetic_key()
        s0 = [p["pair"] for p in key["pairs"] if p["stratum"] == "s0"][:16]
        rest = [p["pair"] for p in key["pairs"] if p["stratum"] != "s0"][:30]
        report = score.score(answers_for(key, set(s0 + rest)), key)
        self.assertEqual(report["correct"], 46)
        self.assertEqual(report["verdict"], "fail")
        self.assertTrue(report["strata"]["s0"]["flagged"])

    def test_a_visibly_wrong_flag_fails_the_session(self):
        key = synthetic_key()
        answers = answers_for(key, set(), p007={"visibly_wrong": True, "note": "radical too short"})
        answers["answers"]["p007"].update({"visibly_wrong": True, "note": "radical too short"})
        report = score.score(answers, key)
        self.assertEqual(report["verdict"], "fail")
        self.assertEqual(report["flagged"][0]["note"], "radical too short")

    def test_an_inverted_session_is_reported_below_chance(self):
        key = synthetic_key()
        report = score.score(answers_for(key, set()), key)
        self.assertEqual((report["correct"], report["verdict"], report["below_chance"]), (0, "pass", True))
        self.assertFalse(score.score(answers_for(key, {p["pair"] for p in key["pairs"][:50]}), key)["below_chance"])

    def test_incomplete_or_foreign_answers_are_refused(self):
        key = synthetic_key()
        answers = answers_for(key, set())
        del answers["answers"]["p003"]
        with self.assertRaisesRegex(ValueError, "unanswered"):
            score.score(answers, key)
        with self.assertRaisesRegex(ValueError, "kit"):
            score.score(dict(answers_for(key, set()), kit="other"), key)

    def test_the_cli_exit_codes(self):
        key = synthetic_key()
        work = pathlib.Path(tempfile.mkdtemp(prefix="fmn-2afc-cli-"))
        self.addCleanup(shutil.rmtree, work, True)
        (work / "key.json").write_text(json.dumps(key))
        for name, answers, code in (("pass", answers_for(key, set()), 0),
                                    ("fail", answers_for(key, {p["pair"] for p in key["pairs"]}), 1),
                                    ("bad", {"kit": "k1", "answers": {}}, 2)):
            (work / f"{name}.json").write_text(json.dumps(answers))
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(score.main([str(work / f"{name}.json"), str(work / "key.json")]), code)


class SamplingTests(unittest.TestCase):
    ROWS = [
        (r"\frac{a}{b}", [r"\frac"], 5), (r"\sqrt{2}", [r"\sqrt"], 3), (r"x^2", ["script:sup"], 50),
        (r"\left( x \right)", [r"\left", r"\right"], 4), (r"\alpha", [r"\alpha"], 9), (r"a = b", [], 7),
        (r"\frac{1}{2} \alpha", [r"\frac", r"\alpha"], 2), ("too long " * 20, [r"\frac"], 99),
        (r"a \\ b", [r"\frac"], 99), (r"a & b", [r"\frac"], 99), ("text", [r"\frac"], 99, "text"),
    ]

    def test_the_draw_is_deterministic_and_seeded(self):
        path = corpus(self.ROWS)
        self.addCleanup(shutil.rmtree, path.parent, True)
        first = kit.sample(path, 7, per_stratum=2, backups=1)
        self.assertEqual(first, kit.sample(path, 7, per_stratum=2, backups=1))
        seen = [d["id"] for items in first.values() for d in items]
        self.assertEqual(len(seen), len(set(seen)), "a formula was drawn for two strata")
        texts = {d["text"] for items in kit.sample(path, 7, per_stratum=20, backups=20).values() for d in items}
        # Too long, multi-line, aligned, and text-mode strings are never drawn.
        self.assertFalse(texts & {("too long " * 20).strip(), r"a \\ b", "a & b", "text"})
        self.assertIn(r"\frac{a}{b}", texts)

    def test_each_stratum_draws_only_its_own_constructs(self):
        path = corpus(self.ROWS)
        self.addCleanup(shutil.rmtree, path.parent, True)
        draws = kit.sample(path, 7, per_stratum=5, backups=0)
        tests = dict(kit.STRATA)
        rows = {f"{i:064x}": r for i, r in enumerate(self.ROWS)}
        for name, items in draws.items():
            for d in items:
                self.assertTrue(tests[name](set(rows[d["id"]][1]), rows[d["id"]][0]), (name, d["text"]))

    def test_occurrence_weighting(self):
        path = corpus([(r"\frac{a}{b}", [r"\frac"], 1000), (r"\frac{c}{d}", [r"\frac"], 1)])
        self.addCleanup(shutil.rmtree, path.parent, True)
        heavy_first = sum(kit.sample(path, seed, per_stratum=1, backups=0)["fractions-radicals"][0]["count"] == 1000
                          for seed in range(200))
        self.assertGreaterEqual(heavy_first, 195)

    def test_sides_and_order(self):
        order, sides = kit.assign(1000, 3)
        self.assertEqual(sorted(order), list(range(1000)))
        self.assertEqual((order, sides), kit.assign(1000, 3))
        self.assertNotEqual(order, kit.assign(1000, 4)[0])
        self.assertTrue(450 <= sum(sides) <= 550)


def png_uri(array):
    buffer = io.BytesIO()
    Image.fromarray(array).save(buffer, format="PNG")
    return base64.b64encode(buffer.getvalue()).decode("ascii")


class ImageAndSimulationTests(unittest.TestCase):
    def setUp(self):
        self.work = pathlib.Path(tempfile.mkdtemp(prefix="fmn-2afc-img-"))
        self.addCleanup(shutil.rmtree, self.work, True)

    def canvas(self, box):
        img = np.full((200, 300, 3), kit.BACKGROUND, np.uint8)
        top, bottom, left, right = box
        img[top:bottom, left:right] = 255
        return img

    def test_crops_share_the_union_box_plus_padding(self):
        for name, box in (("a.png", (90, 110, 100, 140)), ("b.png", (80, 100, 120, 200))):
            Image.fromarray(self.canvas(box)).save(self.work / name)
        a, b = kit.crop_pair(self.work / "a.png", self.work / "b.png")
        self.assertEqual(a.shape, b.shape)
        self.assertEqual(a.shape[:2], (110 - 80 + 2 * kit.PAD, 200 - 100 + 2 * kit.PAD))
        Image.fromarray(np.full((50, 50, 3), kit.BACKGROUND, np.uint8)).save(self.work / "blank.png")
        self.assertIsNone(kit.crop_pair(self.work / "a.png", self.work / "blank.png"))

    def test_render_identity_reads_the_provenance_manifest(self):
        (self.work / "x.png.manifest").mkdir()
        (self.work / "x.png.manifest" / "manifest.txt").write_text(
            'manifest_version = "1.0"\nmode = "certified"\nbuild_id = "git:abc"\n')
        self.assertEqual(kit.render_identity(self.work / "x.png"), ("git:abc", "certified"))

    def make_kit(self, control, latex_left):
        latex = self.canvas((90, 110, 100, 140))
        other = latex if control == "latex-vs-latex" else self.canvas((80, 120, 100, 160))
        blocks, pairs = [], []
        for i, left_is_latex in enumerate(latex_left):
            fid = f"{i + 1:016x}" + "0" * 48  # the kit names pair directories by the first 16
            work = self.work / "pairs" / fid[:16]
            work.mkdir(parents=True)
            Image.fromarray(latex).save(work / "latex.png")
            other_name = {"latex-vs-latex": "latex", "latex-vs-plex": "plex"}.get(control, "fmd")
            if other_name != "latex":
                Image.fromarray(other).save(work / f"{other_name}.png")
            crops = kit.crop_pair(work / "latex.png", work / f"{other_name}.png")
            left, right = (crops[0], crops[1]) if left_is_latex else (crops[1], crops[0])
            pid = f"p{i:03d}"
            blocks.append(kit.PAIR_HTML.format(pid=pid, number=i + 1, left="data:image/png;base64," + png_uri(left),
                                               right="data:image/png;base64," + png_uri(right)))
            pairs.append({"pair": pid, "formula": fid, "stratum": "s0"})
        (self.work / "review.html").write_text("\n".join(blocks))
        (self.work / "manifest.json").write_text(json.dumps({"kit_id": "k", "control": control, "pairs": pairs}))

    def test_latex_match_is_right_whenever_the_sides_differ(self):
        sides = [True, False, False, True, False]
        self.make_kit("latex-vs-plex", sides)
        answers = kit.simulate(self.work, "latex-match", 0)["answers"]
        self.assertEqual([answers[f"p{i:03d}"]["choice"] for i in range(5)],
                         ["left" if s else "right" for s in sides])

    def test_latex_match_can_only_guess_left_against_latex(self):
        self.make_kit("latex-vs-latex", [True, False, True])
        answers = kit.simulate(self.work, "latex-match", 0)["answers"]
        self.assertEqual({a["choice"] for a in answers.values()}, {"left"})

    def test_the_random_reviewer_is_seeded(self):
        self.make_kit("latex-vs-plex", [True] * 40)
        first = kit.simulate(self.work, "random", 5)
        self.assertEqual(first, kit.simulate(self.work, "random", 5))
        lefts = sum(a["choice"] == "left" for a in first["answers"].values())
        self.assertTrue(8 <= lefts <= 32)


if __name__ == "__main__":
    random.seed(0)
    unittest.main()
