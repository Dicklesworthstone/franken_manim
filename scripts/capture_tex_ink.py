#!/usr/bin/env python3
"""Capture the pinned Reference's Tex ink for the ink-fidelity oracle (fm-tex-ink-oracle-5ehf).

Each row is the outline geometry of the Reference's own `Tex(source)` or `TexText(source)` at the
default font_size 48: every family member's shared-anchor quadratic point run, as dvisvgm traced
it, in scene units. The oracle in crates/fmn-conformance/tests/tex_ink_oracle.rs rasterizes these
outlines and fmd-math's through the same certified engine and compares the ink. They are captured
once, offline, through a private TeX cache (Appendix C-22); nothing at test time invokes LaTeX
(D-03). There are two sets:

- public: the authored formulas of the Reference box oracle
  (crates/fmn-library/tests/fixtures/tex_reference_boxes.v1.tsv) plus a few authored TexText
  strings, named by their source, written to crates/fmn-conformance/fixtures/.
- private: the corpus sample of the TeX box oracle (tex_corpus_boxes.v1.tsv, by digest) plus the
  most frequent TexText strings of the corpus. The corpus is CC BY-NC-SA (plan §15.3), and its
  outlines are derived from it, so this set is written beside the corpus (corpus/ is never
  committed). Its rows carry only digests.

A row's `class` is the width capture's: plain (Computer Modern, which fmd-math bundles), traced
(only the EC text fonts, which this host's dvisvgm traced from METAFONT bitmaps, BN-05), or
fallback (another font). Points are integers in units of 1e-4 scene units, x then y, each member's
run delta-coded from its first point; members are separated by `|`.

Usage, from the repository root, with the Reference venv and checkout:
  scripts/capture_tex_ink.py --corpus corpus/tex_corpus.jsonl \\
      --reference-python refenv/bin/python --reference-root scripts/manim_ref
"""
from __future__ import annotations

import argparse
import datetime
import json
import os
import pathlib
import subprocess
import sys
import tempfile

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from capture_tex_reference_widths import digest, dvi_fonts, font_class, tool_versions  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[1]
PUBLIC = ROOT / "crates" / "fmn-conformance" / "fixtures" / "tex_ink_reference.v1.txt"
BOXES = ROOT / "crates" / "fmn-library" / "tests" / "fixtures" / "tex_reference_boxes.v1.tsv"
CORPUS_BOXES = ROOT / "crates" / "fmn-conformance" / "fixtures" / "tex_corpus_boxes.v1.tsv"
FONT_SIZE = 48
UNIT = 1e-4
TEXT_TOP = 60
# Project-authored TexText strings: plain prose, punctuation, bold and italic shapes, digits.
AUTHORED_TEXT = [
    "Hello, world",
    "can't",
    "The quick brown fox jumps over the lazy dog",
    "Fourier series",
    "Area under the curve",
    r"\textbf{Theorem.} Every map is continuous",
    r"\textit{italic} text",
    "Step 1: square both sides",
    "e.g. 3.14159",
    r"50\% of the time",
    "first, second, third",
    "Rotation by 90 degrees",
]


def public_rows() -> list[dict]:
    rows = []
    for line in BOXES.read_text(encoding="utf-8").splitlines():
        if line.startswith("#") or not line.strip():
            continue
        source = line.split("\t", 2)[2]
        rows.append({"kind": "math", "key": source, "text": source, "mode": "math"})
    rows += [{"kind": "text", "key": text, "text": text, "mode": "text"} for text in AUTHORED_TEXT]
    return rows


def private_rows(corpus: pathlib.Path) -> list[dict]:
    records = [json.loads(line) for line in corpus.open(encoding="utf-8")]
    by_digest = {digest(r["mode"], r["text"]): r for r in records}
    rows = []
    for line in CORPUS_BOXES.read_text(encoding="utf-8").splitlines():
        if line.startswith("#") or not line.strip():
            continue
        key = line.split("\t", 1)[0]
        record = by_digest[key]
        rows.append({"kind": "math", "key": key, "text": record["text"], "mode": "math"})
    text = sorted((r for r in records if r["mode"] == "text" and r["text"].strip()),
                  key=lambda r: (-r["count"], digest(r["mode"], r["text"])))
    for r in text[:TEXT_TOP]:
        rows.append({"kind": "text", "key": digest("text", r["text"]), "text": r["text"], "mode": "text"})
    return rows


def capture(candidates_path: pathlib.Path, out_path: pathlib.Path, config: pathlib.Path) -> int:
    """Under the Reference venv: each candidate's outlines and the fonts its DVI loads."""
    sys.argv = ["tex-ink-capture", "--config_file", str(config)]
    from manimlib import Tex, TexText  # noqa: E402 - the Reference parses argv at import
    from manimlib.mobject.svg.tex_mobject import get_tex_mob_scale_factor

    get_tex_mob_scale_factor()
    dvi = pathlib.Path(json.loads(config.read_text())["directories"]["subdirs"]["latex_cache"]) / "working.dvi"
    results = []
    for item in json.loads(candidates_path.read_text(encoding="utf-8")):
        try:
            build = Tex if item["mode"] == "math" else TexText
            mob = build(item["text"], font_size=FONT_SIZE)
            fonts = dvi_fonts(dvi.read_bytes())
            parts = [m.get_points()[:, :2].tolist() for m in mob.family_members_with_points()]
            results.append({"key": item["key"], "kind": item["kind"], "fonts": fonts, "parts": parts})
        except Exception as error:  # noqa: BLE001 - a refusal is recorded, not fatal
            results.append({"key": item["key"], "kind": item["kind"],
                            "error": f"{type(error).__name__}: {str(error)[:200]}"})
    out_path.write_text(json.dumps(results), encoding="utf-8")
    return 0


def encode(parts: list[list[list[float]]]) -> str:
    members = []
    for points in parts:
        ints = [(round(x / UNIT), round(y / UNIT)) for x, y in points]
        out, (px, py) = [f"{ints[0][0]} {ints[0][1]}"], ints[0]
        for x, y in ints[1:]:
            out.append(f"{x - px} {y - py}")
            px, py = x, y
        members.append(" ".join(out))
    return "|".join(members)


def write(path: pathlib.Path, rows: list[dict], measured: dict, reference: str, which: str) -> list[dict]:
    refused = [row for row in rows if "parts" not in measured[(row["kind"], row["key"])]]
    kept = [row for row in rows if measured[(row["kind"], row["key"])].get("parts")]
    bare = len(rows) - len(refused) - len(kept)
    lines = [
        f"# Reference Tex ink for the ink-fidelity oracle (fm-tex-ink-oracle-5ehf), {which} set.",
        f"# Each row is the pinned Reference's Tex(source) (kind math) or TexText(source) (kind text) at the",
        f"# default font_size {FONT_SIZE}: 3b1b/manim {reference[:8]}, {tool_versions()},",
        f"# measured {datetime.date.today().isoformat()} by scripts/capture_tex_ink.py through a private TeX cache.",
        "# key is the source (public set) or sha256(mode NUL text) (private set). class is plain, traced or",
        "# fallback as in tex_reference_widths.v1.tsv. parts: each family member's shared-anchor quadratic",
        f"# point run in units of {UNIT:g} scene units, x y pairs delta-coded from the member's first point,",
        f"# members separated by |. {len(refused)} candidates the Reference could not typeset and {bare} that",
        "# typeset no glyph are omitted. Re-measure only by rerunning the capture; never edit by hand.",
        "# kind\tkey\tclass\tparts",
    ]
    for row in kept:
        m = measured[(row["kind"], row["key"])]
        lines.append(f"{row['kind']}\t{row['key']}\t{font_class(m['fonts'])}\t{encode(m['parts'])}")
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"wrote {path}: {len(kept)} rows ({sum(r['kind'] == 'text' for r in kept)} text), {len(refused)} refused")
    for row in refused:
        error = measured[(row["kind"], row["key"])].get("error", "no glyph")
        label = row["key"] if which == "public" else row["key"][:16]
        print(f"  refused {row['kind']} {label}: {error}", file=sys.stderr)
    return kept


def build(args) -> int:
    rows = public_rows()
    private = private_rows(args.corpus) if args.corpus else []
    with tempfile.TemporaryDirectory(prefix="fmn-tex-ink-") as work:
        work = pathlib.Path(work)
        (work / "candidates.json").write_text(json.dumps(rows + private), encoding="utf-8")
        (work / "latex").mkdir()
        (work / "svg").mkdir()
        config = work / "config.yml"
        config.write_text(json.dumps({"directories": {"cache": str(work / "svg"),
                                                      "subdirs": {"latex_cache": str(work / "latex")}}}))
        env = {"PATH": "/usr/bin:/bin", "HOME": os.environ.get("HOME", "/tmp"),
               "PYTHONPATH": str(args.reference_root.resolve()), "OMP_NUM_THREADS": "1"}
        subprocess.run(["xvfb-run", "-a", "-s", "-screen 0 640x480x24", str(args.reference_python),
                        str(pathlib.Path(__file__).resolve()), "capture", str(work / "candidates.json"),
                        str(work / "ink.json"), str(config)],
                       cwd=work, env=env, check=True, timeout=7200)
        measured = {(r["kind"], r["key"]): r for r in json.loads((work / "ink.json").read_text(encoding="utf-8"))}
    reference = subprocess.run(["git", "-C", str(args.reference_root), "rev-parse", "HEAD"],
                               capture_output=True, text=True, check=True).stdout.strip()
    write(args.out, rows, measured, reference, "public")
    if private:
        write(args.private_out, private, measured, reference, "private")
    return 0


def main(argv=None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if argv and argv[0] == "capture":
        return capture(pathlib.Path(argv[1]), pathlib.Path(argv[2]), pathlib.Path(argv[3]))
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--corpus", type=pathlib.Path, help="the private TeX corpus; omit for the public set only")
    parser.add_argument("--reference-python", type=pathlib.Path, required=True)
    parser.add_argument("--reference-root", type=pathlib.Path, required=True)
    parser.add_argument("--out", type=pathlib.Path, default=PUBLIC)
    parser.add_argument("--private-out", type=pathlib.Path, default=ROOT / "corpus" / "tex_ink_reference.v1.txt")
    return build(parser.parse_args(argv))


if __name__ == "__main__":
    sys.exit(main())
