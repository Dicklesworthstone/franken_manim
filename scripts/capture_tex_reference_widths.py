#!/usr/bin/env python3
"""Capture the pinned Reference's Tex extents for the width oracle
(fm-tex-metrics-glyphs-ru72).

Each row is the Reference's own `Tex(source)` at its default font_size 48 (align* display math):
`get_width()` and `get_height()` in scene units, the numbers a scene author sees. They are captured
once, offline, through a private TeX cache (Appendix C-22), with the Reference's
`get_tex_mob_scale_factor` calibration. There are two kinds of row:

- `authored`: the ten formulas the bead measured first, project-authored, named by their source.
- `corpus`: the most frequent math strings of the private 3b1b corpus. The corpus is CC BY-NC-SA
  (plan §15.3), so a row carries only its digest sha256(mode NUL text) and occurrence count. The
  oracle finds the strings again through the corpus where it is present.

Strings the Reference cannot typeset alone are left out and counted in the header. Re-measure
only by rerunning this script against the pinned Reference; never edit the fixture by hand.

Usage, from the repository root, with the Reference venv and checkout:
  scripts/capture_tex_reference_widths.py --corpus corpus/tex_corpus.jsonl \\
      --reference-python refenv/bin/python --reference-root scripts/manim_ref [--top 200]
"""
from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "crates" / "fmn-conformance" / "fixtures" / "tex_reference_widths.v1.tsv"
FONT_SIZE = 48
# The bead's first measurement (2026-10-04): project-authored formulas.
AUTHORED = [
    r"x \geq 0",
    r"a = b",
    r"f(x)",
    r"x + y",
    r"a \leq b \neq c",
    r"x \quad y",
    r"e^{i\pi} + 1 = 0",
    r"\alpha\beta\gamma",
    r"\sin x + \cos y",
    r"x^2 + y^2 = r^2",
]


def digest(mode: str, text: str) -> str:
    return hashlib.sha256(mode.encode() + b"\0" + text.encode()).hexdigest()


def candidates(corpus: pathlib.Path, top: int) -> list[dict]:
    rows = [{"kind": "authored", "key": text, "text": text, "count": 0} for text in AUTHORED]
    authored = set(AUTHORED)
    records = [json.loads(line) for line in corpus.open(encoding="utf-8")]
    ranked = sorted((r for r in records if r["mode"] == "math" and r["text"].strip()),
                    key=lambda r: (-r["count"], digest(r["mode"], r["text"])))
    for r in ranked:
        if len([row for row in rows if row["kind"] == "corpus"]) >= top:
            break
        if r["text"] in authored:
            continue
        rows.append({"kind": "corpus", "key": digest(r["mode"], r["text"]), "text": r["text"],
                     "count": r["count"]})
    return rows


def dvi_fonts(dvi: bytes) -> list[str]:
    """The fonts a DVI file loads, from its postamble's font definitions."""
    end = len(dvi) - 1
    while end > 0 and dvi[end] == 223:
        end -= 1
    q = int.from_bytes(dvi[end - 4:end], "big")
    if dvi[end - 5] != 249 or dvi[q] != 248:
        raise ValueError("not a DVI postamble")
    at, names = q + 29, []
    while dvi[at] in (243, 244, 245, 246):
        at += 1 + (dvi[at] - 242) + 12
        a, length = dvi[at], dvi[at + 1]
        names.append(dvi[at + 2 + a:at + 2 + a + length].decode("ascii"))
        at += 2 + a + length
    return sorted(set(names))


def font_class(fonts: list[str]) -> str:
    """`plain` when TeX set the formula in Computer Modern (cm*, with or without the EC text
    fonts), whose designs fmd-math bundles; `traced` when it used only EC text fonts, which the
    capture host has no outlines for, so its dvisvgm traced them from METAFONT bitmaps (BN-05); and
    `fallback` when it used another font (msam, msbm, dsrom, rsfs, wasy, tipa, ...), whose glyphs
    fmd-math draws from a fallback face."""
    if not all(re.fullmatch(r"(cm|ec)[a-z]+[0-9]+", f) for f in fonts):
        return "fallback"
    return "traced" if all(f.startswith("ec") for f in fonts) else "plain"


def capture(candidates_path: pathlib.Path, out_path: pathlib.Path, config: pathlib.Path) -> int:
    """Under the Reference venv: each candidate's Tex extent in scene units, and the fonts its DVI
    loads (the Reference compiles each string once into its private LaTeX cache's working.dvi)."""
    sys.argv = ["tex-width-capture", "--config_file", str(config)]
    from manimlib import Tex  # noqa: E402 - the Reference parses argv at import
    from manimlib.mobject.svg.tex_mobject import get_tex_mob_scale_factor

    # The scale calibration compiles its own "0" once; do it first, so that every working.dvi
    # read below is the formula's.
    get_tex_mob_scale_factor()
    dvi = pathlib.Path(json.loads(config.read_text())["directories"]["subdirs"]["latex_cache"]) / "working.dvi"
    results = []
    for item in json.loads(candidates_path.read_text(encoding="utf-8")):
        try:
            mob = Tex(item["text"], font_size=FONT_SIZE)
            fonts = dvi_fonts(dvi.read_bytes())
            results.append({"key": item["key"], "width": float(mob.get_width()),
                            "height": float(mob.get_height()), "fonts": fonts})
        except Exception as error:  # noqa: BLE001 - a refusal is recorded, not fatal
            results.append({"key": item["key"], "error": f"{type(error).__name__}: {str(error)[:200]}"})
    out_path.write_text(json.dumps(results), encoding="utf-8")
    return 0


def tool_versions() -> str:
    def first_line(argv):
        try:
            return subprocess.run(argv, capture_output=True, text=True, timeout=30).stdout.splitlines()[0]
        except (OSError, IndexError, subprocess.TimeoutExpired):
            return "unavailable"
    return f"{first_line(['latex', '--version'])}; {first_line(['dvisvgm', '--version'])}"


def build(args) -> int:
    rows = candidates(args.corpus, args.top)
    with tempfile.TemporaryDirectory(prefix="fmn-tex-widths-") as work:
        work = pathlib.Path(work)
        (work / "candidates.json").write_text(json.dumps(rows), encoding="utf-8")
        (work / "latex").mkdir()
        (work / "svg").mkdir()
        config = work / "config.yml"
        config.write_text(json.dumps({"directories": {"cache": str(work / "svg"),
                                                      "subdirs": {"latex_cache": str(work / "latex")}}}))
        env = {"PATH": "/usr/bin:/bin", "HOME": os.environ.get("HOME", "/tmp"),
               "PYTHONPATH": str(args.reference_root.resolve()), "OMP_NUM_THREADS": "1"}
        subprocess.run(["xvfb-run", "-a", "-s", "-screen 0 640x480x24", str(args.reference_python),
                        str(pathlib.Path(__file__).resolve()), "capture", str(work / "candidates.json"),
                        str(work / "widths.json"), str(config)],
                       cwd=work, env=env, check=True, timeout=7200)
        measured = {r["key"]: r for r in json.loads((work / "widths.json").read_text(encoding="utf-8"))}
    reference = subprocess.run(["git", "-C", str(args.reference_root), "rev-parse", "HEAD"],
                               capture_output=True, text=True, check=True).stdout.strip()
    refused = [row for row in rows if "width" not in measured[row["key"]]]
    # Fragments of multi-argument Tex calls (a lone `\over`, `{}`) typeset no glyph at all.
    bare = [row for row in rows if measured[row["key"]].get("fonts") == []]
    kept = [row for row in rows if measured[row["key"]].get("fonts")]
    lines = [
        "# Reference Tex extents for the width oracle (fm-tex-metrics-glyphs-ru72).",
        f"# Each row is the pinned Reference's Tex(source) get_width()/get_height() in scene units at the",
        f"# default font_size {FONT_SIZE}: 3b1b/manim {reference[:8]}, align* display math, {tool_versions()},",
        f"# measured {datetime.date.today().isoformat()} by scripts/capture_tex_reference_widths.py through a private TeX cache.",
        "# authored rows name their source; corpus rows only the digest sha256(mode NUL text) of a private",
        f"# corpus string, with its occurrence count. {len(refused)} candidates the Reference could not",
        f"# typeset alone, and {len(bare)} fragments that typeset no glyph, are omitted. `class` is plain",
        "# when TeX set the string in Computer Modern (cm*, the EC text fonts), traced when only in EC",
        "# text fonts, which this host's dvisvgm traced from bitmaps, and fallback when its DVI loads any",
        "# other font; `fonts` lists them.",
        "# Re-measure only by rerunning the capture; never edit by hand.",
        "# kind\tkey\tcount\twidth\theight\tclass\tfonts",
    ]
    for row in kept:
        m = measured[row["key"]]
        lines.append(f"{row['kind']}\t{row['key']}\t{row['count']}\t{m['width']:.6f}\t{m['height']:.6f}"
                     f"\t{font_class(m['fonts'])}\t{','.join(m['fonts'])}")
    args.out.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"wrote {args.out}: {len(kept)} rows ({sum(r['kind'] == 'corpus' for r in kept)} corpus), "
          f"{len(refused)} refused")
    for row in refused:
        print(f"  refused {row['kind']} {row['key'][:16]}: {measured[row['key']]['error']}", file=sys.stderr)
    return 0


def main(argv=None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if argv and argv[0] == "capture":
        return capture(pathlib.Path(argv[1]), pathlib.Path(argv[2]), pathlib.Path(argv[3]))
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--corpus", type=pathlib.Path, required=True)
    parser.add_argument("--reference-python", type=pathlib.Path, required=True)
    parser.add_argument("--reference-root", type=pathlib.Path, required=True)
    parser.add_argument("--top", type=int, default=200)
    parser.add_argument("--out", type=pathlib.Path, default=FIXTURE)
    return build(parser.parse_args(argv))


if __name__ == "__main__":
    sys.exit(main())
