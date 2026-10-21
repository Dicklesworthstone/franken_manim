#!/usr/bin/env python3
"""Capture the pinned Reference's Tex substring selections (fm-portal-span-parity-sc4k).

For each authored formula of the Reference box oracle
(crates/fmn-library/tests/fixtures/tex_reference_boxes.v1.tsv) and a deterministic set of
selectors, this records which glyphs the Reference selects three ways:

- `getitem`: `Tex(s)[sel]`. For a selector the formula does not isolate, the Reference maps each
  match to submobjects by counting TeX symbols before it (`select_unisolated_substring` with
  `substr_to_path_count`).
- `t2c`: the submobjects `Tex(s, t2c={sel: RED})` colours (string selectors: the Reference's
  t2c takes string keys only).
- `isolate`: `Tex(s, isolate=[sel])[sel]`, the labelled-span path.

It also records `Tex(s).substr_to_path_count(sel)` for string selectors, and, for 50 authored
formula pairs, how `TransformMatchingTex(Tex(a), Tex(b))` pairs their pieces: each
sub-animation's kind (transform, fade_out, fade_in) and the centres of its source and target
pieces. That is recorded twice: as the Reference runs it (`matching`), and with its leftover
shape matching (`find_pairs_with_matching_shapes`) disabled (`matching_blocks`). The portal
deliberately drops shape matching, so it should equal the second.

Submobject orders differ between engines (fm-aia1), so glyphs are compared by position, not
index. Each formula records its submobjects' boxes (centre x, y, width, height), and a selection
records the indices it picks with the boxes of the formula it picked from. The Reference's t2c and
isolate re-typeset the formula with the selector braced, which can move glyphs (an isolated
`\\sum` takes its limits as scripts), so a comparison maps those indices back to the plain
formula's boxes where the two have the same glyphs.

The formulas are project-authored, so the fixture is public. Captured once, offline, through a
private TeX cache (Appendix C-22). Re-measure only by rerunning; never edit by hand.

Usage, from the repository root:
  scripts/capture_tex_selection.py --reference-python refenv/bin/python --reference-root scripts/manim_ref
"""
from __future__ import annotations

import argparse
import datetime
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
BOXES = ROOT / "crates" / "fmn-library" / "tests" / "fixtures" / "tex_reference_boxes.v1.tsv"
FIXTURE = ROOT / "crates" / "fmn-conformance" / "fixtures" / "tex_selection_reference.v1.ndjson"
# Commands that take arguments, open environments or only space or style: as selectors they
# name no glyph of their own.
STRUCTURAL = {
    r"\frac", r"\dfrac", r"\tfrac", r"\sqrt", r"\left", r"\right", r"\begin", r"\end", r"\binom",
    r"\quad", r"\qquad", r"\,", r"\;", r"\!", r"\:", r"\text", r"\mathbb", r"\mathcal", r"\mathrm",
    r"\mathbf", r"\hat", r"\vec", r"\bar", r"\dot", r"\ddot", r"\tilde", r"\overline", r"\underline",
    r"\overbrace", r"\underbrace", r"\pmod", r"\\", r"\big", r"\Big",
}
TOKEN = re.compile(r"\\[A-Za-z]+|\\[^A-Za-z]|[A-Za-z0-9]|[-+=<>()\[\]|!,./']")
# Authored rewrites for TransformMatchingTex, beside neighbouring box-oracle formulas.
REWRITES = [
    ("a^2 + b^2 = c^2", "c^2 = a^2 + b^2"),
    (r"e^{i\pi} + 1 = 0", r"e^{i\pi} = -1"),
    (r"\frac{a}{b}", r"\frac{b}{a}"),
    ("x + y", "y + x"),
    ("2x + 3 = 7", "2x = 4"),
    ("2x = 4", "x = 2"),
    (r"\sin^2\theta + \cos^2\theta = 1", r"\cos^2\theta = 1 - \sin^2\theta"),
    ("(a + b)^2", "a^2 + 2ab + b^2"),
    ("f(x) = x^2", "f'(x) = 2x"),
    ("y = mx + b", "y - b = mx"),
    ("E = mc^2", "m = E / c^2"),
    (r"a \cdot b = c", "b = c / a"),
    (r"\sqrt{x^2 + y^2}", "x^2 + y^2"),
    (r"\sum_{k=1}^{n} k", r"\frac{n(n+1)}{2}"),
    ("A = \\pi r^2", "r = \\sqrt{A / \\pi}"),
    ("x^2 - 1", "(x - 1)(x + 1)"),
    ("a + b + c", "c + b + a"),
    ("3 + 4 = 7", "4 + 3 = 7"),
    (r"\alpha + \beta", r"\beta + \alpha"),
    ("F = ma", "a = F / m"),
]


def formulas() -> list[str]:
    out = []
    for line in BOXES.read_text(encoding="utf-8").splitlines():
        if line.startswith("#") or not line.strip():
            continue
        out.append(line.split("\t", 2)[2])
    return out


def selectors(source: str) -> list[dict]:
    """Symbol tokens in order of first appearance, then the space-separated terms longer than one
    token whose braces balance, then one regex (digits, or letters when there are none). The
    brackets of `\\sqrt[n]` and environment names are command syntax, not glyphs, and are left
    out."""
    seen, out = set(), []
    syntax = {"[", "]"} if r"\sqrt[" in source else set()
    # An environment's name is syntax too.
    for token in TOKEN.findall(re.sub(r"\\(begin|end)\{[^}]*\}", " ", source)):
        if token in STRUCTURAL or token in seen or token in syntax:
            continue
        seen.add(token)
        out.append({"kind": "str", "value": token})
    def balanced(term):
        depth = 0
        for char in term:
            depth += {"{": 1, "}": -1}.get(char, 0)
            if depth < 0:
                return False
        return depth == 0

    for term in source.split():
        if term in seen or len(TOKEN.findall(term)) < 2 or not balanced(term):
            continue
        if any(term.startswith(command) for command in (r"\begin", r"\end", r"\left", r"\right")):
            continue
        seen.add(term)
        out.append({"kind": "str", "value": term})
    pattern = r"[0-9]" if re.search(r"[0-9]", source) else r"[a-z]"
    out.append({"kind": "regex", "value": pattern})
    return out


def pairs(sources: list[str]) -> list[list[str]]:
    """30 neighbouring box-oracle formulas, then the authored rewrites."""
    return [list(pair) for pair in zip(sources[:30], sources[1:31])] + [list(p) for p in REWRITES]


def capture(candidates_path: pathlib.Path, out_path: pathlib.Path, config: pathlib.Path) -> int:
    """Under the Reference venv: the selections of every (formula, selector, mode), and the
    TransformMatchingTex pairings of every pair."""
    sys.argv = ["tex-selection-capture", "--config_file", str(config)]
    from manimlib import RED, Tex, TransformMatchingTex  # noqa: E402 - the Reference parses argv
    from manimlib.animation.transform_matching_parts import TransformMatchingParts

    red = Tex("x", t2c={"x": RED})[0].get_fill_color().lower()

    def centres(mobjects):
        return [[round(float(c), 5) for c in m.get_center()[:2]] for m in mobjects]

    def boxes(mobjects):
        return [[round(float(c), 5) for c in (*m.get_center()[:2], m.get_width(), m.get_height())]
                for m in mobjects]

    def chosen(tex, group):
        family = {id(m) for m in group.get_family()}
        return [i for i, m in enumerate(tex.submobjects) if id(m) in family]

    def pieces(mobject):
        return boxes(mobject.family_members_with_points())

    def pairing(first, second):
        anims = []
        for anim in TransformMatchingTex(Tex(first), Tex(second)).anims:
            name = type(anim).__name__
            if name == "FadeOutToPoint":
                anims.append({"kind": "fade_out", "source": pieces(anim.mobject), "target": []})
            elif name == "FadeInFromPoint":
                anims.append({"kind": "fade_in", "source": [], "target": pieces(anim.mobject)})
            else:
                anims.append({"kind": "transform", "source": pieces(anim.mobject),
                              "target": pieces(anim.target_mobject),
                              "same_shape": bool(anim.mobject.has_same_shape_as(anim.target_mobject))})
        return anims

    rows = []
    candidates = json.loads(candidates_path.read_text(encoding="utf-8"))
    shapes = TransformMatchingParts.find_pairs_with_matching_shapes
    for first, second in candidates["pairs"]:
        record = {"pair": [first, second]}
        for mode, matcher in (("matching", shapes), ("matching_blocks", lambda self, a, b: [])):
            TransformMatchingParts.find_pairs_with_matching_shapes = matcher
            try:
                rows.append({**record, "mode": mode, "anims": pairing(first, second)})
            except Exception as error:  # noqa: BLE001 - a refusal is recorded, not fatal
                rows.append({**record, "mode": mode, "error": f"{type(error).__name__}: {str(error)[:160]}"})
        TransformMatchingParts.find_pairs_with_matching_shapes = shapes
    for item in candidates["formulas"]:
        source = item["string"]
        try:
            plain = Tex(source)
        except Exception as error:  # noqa: BLE001 - a refusal is recorded, not fatal
            rows.append({"string": source, "error": f"{type(error).__name__}: {str(error)[:160]}"})
            continue
        rows.append({"string": source, "boxes": boxes(plain.submobjects)})
        for selector in item["selectors"]:
            value = selector["value"]
            key = re.compile(value) if selector["kind"] == "regex" else value
            # The Reference's t2c takes string keys only (a pattern key raises TypeError).
            modes = ("getitem", "t2c", "isolate") if selector["kind"] == "str" else ("getitem", "isolate")
            for mode in modes:
                record = {"string": source, "selector": selector, "mode": mode}
                try:
                    if mode == "getitem":
                        tex, picked = plain, chosen(plain, plain[key])
                    elif mode == "t2c":
                        tex = Tex(source, t2c={key: RED})
                        picked = [i for i, m in enumerate(tex.submobjects)
                                  if m.get_fill_color().lower() == red]
                    else:
                        tex = Tex(source, isolate=[key])
                        picked = chosen(tex, tex[key])
                    record["selected"] = picked
                    if mode != "getitem":
                        record["boxes"] = boxes(tex.submobjects)
                except Exception as error:  # noqa: BLE001 - the Reference refused this selection
                    record["error"] = f"{type(error).__name__}: {str(error)[:160]}"
                rows.append(record)
            if selector["kind"] == "str":
                try:
                    count = int(plain.substr_to_path_count(value))
                except Exception as error:  # noqa: BLE001
                    count = f"{type(error).__name__}: {str(error)[:120]}"
                rows.append({"string": source, "selector": selector, "mode": "count", "count": count})
    out_path.write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows), encoding="utf-8")
    return 0


def tool_versions() -> str:
    def first_line(argv):
        try:
            return subprocess.run(argv, capture_output=True, text=True, timeout=30).stdout.splitlines()[0]
        except (OSError, IndexError, subprocess.TimeoutExpired):
            return "unavailable"
    return f"{first_line(['latex', '--version'])}; {first_line(['dvisvgm', '--version'])}"


def build(args) -> int:
    items = [{"string": s, "selectors": selectors(s)} for s in formulas()]
    matching = pairs(formulas())
    with tempfile.TemporaryDirectory(prefix="fmn-tex-selection-") as work:
        work = pathlib.Path(work)
        (work / "candidates.json").write_text(json.dumps({"formulas": items, "pairs": matching}),
                                              encoding="utf-8")
        (work / "latex").mkdir()
        (work / "svg").mkdir()
        config = work / "config.yml"
        config.write_text(json.dumps({"directories": {"cache": str(work / "svg"),
                                                      "subdirs": {"latex_cache": str(work / "latex")}}}))
        env = {"PATH": "/usr/bin:/bin", "HOME": os.environ.get("HOME", "/tmp"),
               "PYTHONPATH": str(args.reference_root.resolve()), "OMP_NUM_THREADS": "1"}
        subprocess.run(["xvfb-run", "-a", "-s", "-screen 0 640x480x24", str(args.reference_python),
                        str(pathlib.Path(__file__).resolve()), "capture", str(work / "candidates.json"),
                        str(work / "rows.ndjson"), str(config)],
                       cwd=work, env=env, check=True, timeout=7200)
        rows = (work / "rows.ndjson").read_text(encoding="utf-8")
    reference = subprocess.run(["git", "-C", str(args.reference_root), "rev-parse", "HEAD"],
                               capture_output=True, text=True, check=True).stdout.strip()
    header = {"schema": "fmn.tex-selection-reference", "version": 1,
              "reference": f"3b1b/manim@{reference}", "tools": tool_versions(),
              "measured": datetime.date.today().isoformat(),
              "script": "scripts/capture_tex_selection.py",
              "formulas": len(items), "selectors": sum(len(i["selectors"]) for i in items),
              "pairs": len(matching)}
    args.out.write_text(json.dumps(header) + "\n" + rows, encoding="utf-8")
    print(f"wrote {args.out}: {header['formulas']} formulas, {header['selectors']} selectors, "
          f"{header['pairs']} matching pairs")
    return 0


def main(argv=None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if argv and argv[0] == "capture":
        return capture(pathlib.Path(argv[1]), pathlib.Path(argv[2]), pathlib.Path(argv[3]))
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--reference-python", type=pathlib.Path, required=True)
    parser.add_argument("--reference-root", type=pathlib.Path, required=True)
    parser.add_argument("--out", type=pathlib.Path, default=FIXTURE)
    return build(parser.parse_args(argv))


if __name__ == "__main__":
    sys.exit(main())
