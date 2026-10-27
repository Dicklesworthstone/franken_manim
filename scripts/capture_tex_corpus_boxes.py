#!/usr/bin/env python3
"""Capture real TeX's box dimensions for every math-mode corpus string (the census,
fm-box-oracle-full-corpus-r11-lykz) and its stratified sample (fm-tex-layout-oracle-bkbc).

TeX itself reports each formula's box: width, height and depth, from \\wd,
\\ht and \\dp of a saved box, in display style. The preamble is the pinned
Reference's default template (scripts/manim_ref/manimlib/tex_templates.yml,
packages and \\minus), and the document class is 10 pt, so 1 em = 10 pt.
Each string is stripped, as the Reference strips it. A single line is braced
as amsmath braces an align* cell; multi-line strings (`\\\\` or `&`) are
boxed as a top-aligned `aligned`, the inner form of the Reference's align*
wrapper, without its invisible row struts and trailing column glue. amsmath's
\\if@display is set, as align*'s \\everydisplay sets it. No SVG
conversion or Reference SVG cache is involved, so the Reference's cache race
(Appendix C-22, fm-0v8k) cannot reach these boxes.

The corpus is a private fixture (plan §15.3): its strings never ship. The
committed fixtures `crates/fmn-conformance/fixtures/tex_corpus_boxes_census.v1.tsv`
(every math string) and `tex_corpus_boxes.v1.tsv` (the sample) carry only each string's public digest sha256(mode + NUL + string), the
box in ems, its occurrence count and its construct classes. The oracle test
(`crates/fmn-conformance/tests/tex_corpus_boxes.rs`) finds the strings again
through the private corpus where it is present.

Census: every math-mode corpus string. Sample, a subset of the same
measurements: up to PER_CLASS per construct class (by occurrence, digest
tie-break), then the most frequent remaining strings until TARGET candidates.
The sample stays the ink capture's input and the quick-run set. Strings TeX cannot typeset alone (fragments of
multi-argument Tex calls) are left out and counted.

With --text, the text-mode strings too: each one-line string's box
(tex_text_boxes_census.v1.tsv), and each multi-line or wrapping string as the
Reference's \\centering paragraph at 345 pt, with its line count, widest line
and box (tex_text_paragraphs_census.v1.tsv).

Usage:
  capture_tex_corpus_boxes.py --corpus corpus/tex_corpus.jsonl [--jobs 8] [--sample-only]
  (needs `latex` from the TeX installation the Reference uses)
"""
from __future__ import annotations

import argparse
import concurrent.futures
import json
import pathlib
import re
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "crates" / "fmn-conformance" / "fixtures" / "tex_corpus_boxes.v1.tsv"
CENSUS = ROOT / "crates" / "fmn-conformance" / "fixtures" / "tex_corpus_boxes_census.v1.tsv"
TEXT_CENSUS = ROOT / "crates" / "fmn-conformance" / "fixtures" / "tex_text_boxes_census.v1.tsv"
PARAGRAPH_CENSUS = ROOT / "crates" / "fmn-conformance" / "fixtures" / "tex_text_paragraphs_census.v1.tsv"
# The Reference's standalone[preview] page: \linewidth and \parindent under its template.
LINE_WIDTH_EM = 34.5
TEMPLATES = ROOT / "scripts" / "manim_ref" / "manimlib" / "tex_templates.yml"
PER_CLASS = 4
TARGET = 640
BOX = re.compile(r"FMNBOX ([0-9.]+)pt ([0-9.]+)pt ([0-9.]+)pt")


def ranked_math(corpus: pathlib.Path) -> list[dict]:
    """Every math-mode corpus string, most frequent first (digest tie-break)."""
    records = [json.loads(line) for line in corpus.open(encoding="utf-8")]
    math = [r for r in records if r["mode"] == "math" and r["text"].strip()]
    return sorted(math, key=lambda r: (-r["count"], r["sha256"]))


def sample(ranked: list[dict]) -> list[dict]:
    classes: dict[str, list[dict]] = {}
    for r in ranked:
        for c in r["constructs"] or ["(plain)"]:
            classes.setdefault(c, []).append(r)
    chosen: dict[str, dict] = {}
    for c in sorted(classes):
        for r in classes[c][:PER_CLASS]:
            chosen.setdefault(r["sha256"], r)
    for r in ranked:
        if len(chosen) >= TARGET:
            break
        chosen.setdefault(r["sha256"], r)
    return sorted(chosen.values(), key=lambda r: (-r["count"], r["sha256"]))


def reference_preamble() -> str:
    """The `default:` template's preamble block, verbatim."""
    text = TEMPLATES.read_text(encoding="utf-8")
    block = text[text.index("\ndefault:"):].split("preamble: |-\n", 1)[1]
    lines = []
    for line in block.splitlines():
        if line and not line.startswith("    "):
            break
        lines.append(line[4:])
    return "\n".join(lines).strip()


def measure_one(text: str, preamble: str) -> tuple[float, float, float] | None:
    # The Reference strips the joined Tex string; a leading newline would
    # otherwise put a blank line (\par) inside the math.
    text = text.strip()
    multiline = "\\\\" in text or "&" in text
    # A single line is braced as amsmath braces an align* cell
    # ($\m@th\displaystyle{##}$): a top-level \over then forms a display
    # fraction instead of swallowing \displaystyle into its numerator. As in
    # the Reference, a newline follows the text, so a trailing `\` is a
    # control space.
    # Multi-line strings are set as `aligned`, top-aligned ([t]) so the box
    # keeps its first row's baseline, as the Reference's align* rows do; a
    # centred `aligned` clips a short row's negative depth.
    body = (
        f"\\begin{{aligned}}[t]\n{text}\n\\end{{aligned}}" if multiline else f"{{{text}\n}}"
    )
    # amsmath's alignment rows carry an invisible strut (\strut@) that pads
    # the first row's height and the last row's depth. The Reference measures
    # ink, so that padding never reaches the screen; drop it. Row pitch
    # (\baselineskip + \jot) is unaffected. Array struts stay: they are an
    # array's visible row spacing. `aligned` also ends a one-row string's
    # last column with \minalignsep (10 pt) of glue that is no ink; zero it.
    # It is the space between column pairs too, and no sampled row has two
    # top-level pairs.
    unpad = (
        "\\makeatletter\\def\\strut@{}\\makeatother\\renewcommand{\\minalignsep}{0pt}\n"
        if multiline
        else ""
    )
    # align* is display math: its \everydisplay sets amsmath's \if@display,
    # and the inline math of each cell keeps it. The box below is inline
    # math, so set it here, or \mod and \pod take their inline kerns (12 mu
    # and 8 mu for 18 mu), which the Reference never sets (fm-eo8l).
    document = (
        "\\documentclass{article}\n" + preamble + "\n" + unpad
        + "\\newsavebox\\fmnbox\n\\begin{document}\n"
        "\\makeatletter\\@displaytrue\\makeatother\n"
        "\\sbox\\fmnbox{$\\displaystyle\n" + body + "\n$}\n"
        "\\typeout{FMNBOX \\the\\wd\\fmnbox\\space\\the\\ht\\fmnbox\\space\\the\\dp\\fmnbox}\n"
        "\\end{document}\n"
    )
    with tempfile.TemporaryDirectory(prefix="tex-corpus-box-") as work:
        (pathlib.Path(work) / "f.tex").write_text(document, encoding="utf-8")
        try:
            run = subprocess.run(["latex", "-interaction=batchmode", "-halt-on-error", "f.tex"],
                                 cwd=work, capture_output=True, timeout=60, check=False)
        except subprocess.TimeoutExpired:
            return None
        log = (pathlib.Path(work) / "f.log").read_text(encoding="latin-1", errors="replace")
    match = BOX.search(log)
    if run.returncode != 0 or match is None:
        return None
    return tuple(float(v) / 10.0 for v in match.groups())  # pt -> em at 10 pt


def ranked_text(corpus: pathlib.Path) -> list[dict]:
    """Every text-mode corpus string, most frequent first (digest tie-break)."""
    records = [json.loads(line) for line in corpus.open(encoding="utf-8")]
    text = [r for r in records if r["mode"] == "text" and r["text"].strip()]
    return sorted(text, key=lambda r: (-r["count"], r["sha256"]))


MULTILINE = re.compile(r"\\\\|\\par\b|\n\s*\n")


def measure_text(text: str, preamble: str) -> tuple[float, float, float] | str:
    """A TexText string's box in the Reference's document, or why it has none.

    The Reference sets TexText as `\\centering` paragraph text in a
    `standalone[preview]` page (\\linewidth 345 pt, \\parindent 0), after
    stripping the joined string. A string that stays on one line is its
    natural `\\sbox`: the paragraph's single line, at natural interword glue.
    The paragraph's end drops its last glue (the newline after the string,
    or a final control space), so the box ends with `\\unskip`.
    `multi-line` strings break lines themselves; a `wraps` string is wider
    than the line and breaks there; `fragment` strings TeX cannot typeset alone.
    """
    text = text.strip()
    if MULTILINE.search(text):
        return "multi-line"
    document = (
        "\\documentclass[preview]{standalone}\n" + preamble + "\n"
        "\\newsavebox\\fmnbox\n\\begin{document}\n"
        "\\sbox\\fmnbox{" + text + "\n\\unskip}\n"
        "\\typeout{FMNBOX \\the\\wd\\fmnbox\\space\\the\\ht\\fmnbox\\space\\the\\dp\\fmnbox}\n"
        "\\end{document}\n"
    )
    with tempfile.TemporaryDirectory(prefix="tex-corpus-text-box-") as work:
        (pathlib.Path(work) / "f.tex").write_text(document, encoding="utf-8")
        try:
            run = subprocess.run(["latex", "-interaction=batchmode", "-halt-on-error", "f.tex"],
                                 cwd=work, capture_output=True, timeout=60, check=False)
        except subprocess.TimeoutExpired:
            return "fragment"
        log = (pathlib.Path(work) / "f.log").read_text(encoding="latin-1", errors="replace")
    match = BOX.search(log)
    if run.returncode != 0 or match is None:
        return "fragment"
    width, height, depth = (float(v) / 10.0 for v in match.groups())  # pt -> em at 10 pt
    if width > LINE_WIDTH_EM:
        return "wraps"
    return (width, height, depth)


def write_text_census(items: list[dict], measured: dict) -> None:
    rows = []
    omitted = {"multi-line": 0, "wraps": 0, "fragment": 0}
    for item in items:
        box = measured[item["sha256"]]
        if isinstance(box, str) or box[0] <= 0.0 or box[1] + box[2] <= 0.0:
            omitted[box if isinstance(box, str) else "fragment"] += 1
            continue
        classes = "|".join(sorted(item["constructs"])) or "(plain)"
        width, height, depth = box
        rows.append(f"{item['sha256']}\t{width:.5f}\t{height:.5f}\t{depth:.5f}\t{item['count']}\t{classes}")
    header = [
        "# Real TeX boxes for every one-line text-mode corpus string (fm-texttext-box-oracle-r11-hkdv).",
        "# Strings are private fixtures (plan §15.3): each row names its corpus string by",
        "# sha256(mode + NUL + string). Width, height and depth are TeX's \\wd, \\ht and \\dp of the",
        "# stripped string's \\sbox, in ems at 10 pt, in the Reference's standalone[preview] document",
        "# (\\linewidth 345 pt, \\parindent 0) with its default template's preamble: the one line",
        "# its \\centering paragraph sets, at natural interword glue, without the final glue the",
        "# paragraph's end drops.",
        "# Preamble: the pinned Reference's default template (3b1b/manim 6199a00d); TeX Live",
        "# 2025 on the dev host.",
        f"# Census: every text-mode string of the frozen G0-4 corpus, {len(items)} candidates.",
        f"# Omitted: {omitted['multi-line']} multi-line (\\\\, \\par or a blank line), {omitted['wraps']}",
        f"# wider than the line (the Reference breaks them), {omitted['fragment']} TeX could not typeset alone.",
        "# The multi-line and wider strings are measured as paragraphs in tex_text_paragraphs_census.v1.tsv.",
        "# Regenerate only with scripts/capture_tex_corpus_boxes.py; never edit by hand.",
        "# sha256\twidth\theight\tdepth\tcount\tclasses (|-separated construct names)",
    ]
    TEXT_CENSUS.write_text("\n".join(header + rows) + "\n", encoding="utf-8")
    print(f"{len(rows)} rows, omitted {omitted} -> {TEXT_CENSUS.relative_to(ROOT)}")


# Peels a \vbox's lines off its end, reporting each line's natural width (its
# \unhcopy: the material plus the ragged skips' natural widths, which are 0;
# a \vbox line its own width) and its height and depth. e-TeX's
# \lastnodetype steps over the glue, kerns and penalties between lines
# (several where `\\` ends a paragraph); anything else ends the peel. The
# glue and kerns below the last line and above the first (a list's \topsep, a
# display's skips) are summed into \fmntrail and \fmnlead: they set no ink.
PEEL = r"""
\newcount\fmntype \newdimen\fmnlead \newdimen\fmntrail \newif\iffmnseen
\def\fmnline{\setbox4\lastbox
  \ifnum\fmntype=1 \setbox6\hbox{\unhcopy4}\else\setbox6\hbox{\copy4}\fi
  \immediate\write16{FMNLINE \the\wd6 / \the\ht4 / \the\dp4}%
  \global\fmnseentrue \global\fmnlead=0pt }
\def\fmnspace#1{\iffmnseen\global\advance\fmnlead#1\else\global\advance\fmntrail#1\fi}
\def\fmnpeel{\fmntype=\lastnodetype
  \let\fmnnext\fmnpeel
  \ifnum\fmntype=1 \fmnline
  \else\ifnum\fmntype=2 \fmnline
  \else\ifnum\fmntype=11 \fmnspace\lastskip \unskip
  \else\ifnum\fmntype=12 \fmnspace\lastkern \unkern
  \else\ifnum\fmntype=13 \unpenalty
  \else\let\fmnnext\relax
  \fi\fi\fi\fi\fi
  \fmnnext}
"""
LINE = re.compile(r"FMNLINE ([0-9.]+)pt ?/ ?([0-9.]+)pt ?/ ?([0-9.]+)pt")
PARAGRAPH = re.compile(r"FMNPAR ([0-9.]+)pt ?/ ?([0-9.]+)pt")
GAPS = re.compile(r"FMNGAPS (-?[0-9.]+)pt ?/ ?(-?[0-9.]+)pt")
OVERFULL = re.compile(r"Overfull \\hbox \(([0-9.]+)pt too wide\) in paragraph")
SET_LINE = re.compile(r"^\.\\[hv]box\([^)]*\)x[0-9.]+(?:, glue set (- )?([0-9.]+)(fil+)?)?", re.M)


def measure_paragraph(text: str, preamble: str) -> tuple[int, float, float, float] | str:
    """A multi-line or wrapping TexText string as the Reference sets it.

    The paragraph is the Reference's: `\\centering` text at \\linewidth (345 pt)
    in a `standalone[preview]` page under its default template, boxed with
    \\vbox. Returns the line count, the widest line as TeX sets it, the first
    line's height and the depth below its baseline of everything to the last
    line's bottom, in ems at 10 pt. A line is its natural width when the
    ragged skips take its slack, the line width when its glue shrinks to fit,
    and the line width plus TeX's reported excess when it is overfull. Glue
    above the first line and below the last (a list's \\topsep, a display's
    skips) sets no ink and is left out.
    """
    text = text.strip()
    document = (
        "\\documentclass[preview]{standalone}\n" + preamble + "\n" + PEEL
        + "\\begin{document}\n\\hfuzz=0pt \\hbadness=10000 \\showboxdepth=1 \\showboxbreadth=100000\n"
        "\\setbox0\\vbox{\\hsize\\linewidth\\centering\n" + text + "\n\\par}\n"
        "\\immediate\\write16{FMNPAR \\the\\ht0 / \\the\\dp0}\n\\showbox0\n"
        "\\setbox2\\vbox{\\unvcopy0 \\fmnpeel}\n"
        "\\immediate\\write16{FMNGAPS \\the\\fmnlead / \\the\\fmntrail}\n\\end{document}\n"
    )
    with tempfile.TemporaryDirectory(prefix="tex-corpus-paragraph-") as work:
        (pathlib.Path(work) / "f.tex").write_text(document, encoding="utf-8")
        try:
            subprocess.run(["latex", "-interaction=batchmode", "f.tex"],
                           cwd=work, capture_output=True, timeout=60, check=False)
        except subprocess.TimeoutExpired:
            return "fragment"
        log = (pathlib.Path(work) / "f.log").read_text(encoding="latin-1", errors="replace")
    # \showbox stops as an error, so latex exits nonzero either way; a real
    # error is any other `!` message.
    errors = [line for line in log.splitlines() if line.startswith("! ") and line != "! OK."]
    paragraph, gaps = PARAGRAPH.search(log), GAPS.search(log)
    if paragraph is None or gaps is None or errors:
        return "fragment"
    lines = [tuple(float(v) for v in m) for m in LINE.findall(log)][::-1]
    naturals = [line[0] for line in lines]
    shown = SET_LINE.findall(log[log.find("> \\box0="):])
    if not naturals or len(shown) != len(naturals):
        return "fragment"
    excess = iter(float(v) for v in OVERFULL.findall(log))
    widest = 0.0
    for natural, (shrinking, ratio, order) in zip(naturals, shown):
        if shrinking and not order:
            # Shrunk to the line, or overfull past its whole shrink.
            width = LINE_WIDTH_EM * 10.0 + (next(excess, 0.0) if float(ratio) >= 1.0 else 0.0)
        else:
            width = natural
        widest = max(widest, width)
    lead, trail = (float(v) for v in gaps.groups())
    extent = sum(float(v) for v in paragraph.groups()) - lead - trail
    first_height = lines[0][1]
    return (len(naturals), widest / 10.0, first_height / 10.0, (extent - first_height) / 10.0)


def write_paragraph_census(items: list[dict], measured: dict) -> None:
    rows, fragments = [], 0
    for item in items:
        box = measured[item["sha256"]]
        if isinstance(box, str) or box[1] <= 0.0 or box[2] + box[3] <= 0.0:
            fragments += 1
            continue
        classes = "|".join(sorted(item["constructs"])) or "(plain)"
        lines, width, height, depth = box
        rows.append(
            f"{item['sha256']}\t{lines}\t{width:.5f}\t{height:.5f}\t{depth:.5f}\t{item['count']}\t{classes}"
        )
    header = [
        "# Real TeX paragraphs for every multi-line or wrapping text-mode corpus string",
        "# (fm-texttext-line-breaking-r11-bfdx). Strings are private fixtures (plan §15.3): each row",
        "# names its corpus string by sha256(mode + NUL + string). Each string is the Reference's",
        "# TexText paragraph: \\centering text at \\linewidth 345 pt in its standalone[preview]",
        "# document with its default template's preamble, boxed with \\vbox. `lines` counts the lines",
        "# TeX sets; `width` is the widest as set (natural, the line width when its glue shrinks to",
        "# fit, plus TeX's reported excess when overfull); height is the first line's, and depth",
        "# reaches from its baseline to the last line's bottom, without the glue above the first",
        "# line and below the last (a list's \\topsep, a display's skips), in ems at 10 pt.",
        "# Preamble: the pinned Reference's default template (3b1b/manim 6199a00d); TeX Live",
        "# 2025 on the dev host.",
        f"# Census: every text-mode string of the frozen G0-4 corpus with \\\\, \\par or a blank line, or",
        f"# wider than the line: {len(items)} candidates, {fragments} TeX could not typeset alone.",
        "# Regenerate only with scripts/capture_tex_corpus_boxes.py; never edit by hand.",
        "# sha256\tlines\twidth\theight\tdepth\tcount\tclasses (|-separated construct names)",
    ]
    PARAGRAPH_CENSUS.write_text("\n".join(header + rows) + "\n", encoding="utf-8")
    print(f"{len(rows)} rows, {fragments} fragments -> {PARAGRAPH_CENSUS.relative_to(ROOT)}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--corpus", type=pathlib.Path, required=True)
    parser.add_argument("--jobs", type=int, default=8)
    parser.add_argument(
        "--sample-only",
        action="store_true",
        help="measure and write only the stratified sample (a quick run), not the census",
    )
    parser.add_argument(
        "--text",
        action="store_true",
        help="also measure the text-mode census (tex_text_boxes_census.v1.tsv)",
    )
    parser.add_argument(
        "--text-only",
        action="store_true",
        help="measure and write only the text-mode census",
    )
    args = parser.parse_args()
    preamble = reference_preamble()
    if args.text or args.text_only:
        items = ranked_text(args.corpus)
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
            boxes = list(pool.map(lambda item: measure_text(item["text"], preamble), items))
        measured = {item["sha256"]: box for item, box in zip(items, boxes)}
        write_text_census(items, measured)
        # The strings the one-line census omits as multi-line or wider than
        # the line are measured again as the Reference's paragraphs.
        paragraphs = [item for item in items if measured[item["sha256"]] in ("multi-line", "wraps")]
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
            laid = list(pool.map(lambda item: measure_paragraph(item["text"], preamble), paragraphs))
        write_paragraph_census(paragraphs, {item["sha256"]: box for item, box in zip(paragraphs, laid)})
        if args.text_only:
            return 0
    ranked = ranked_math(args.corpus)
    chosen = sample(ranked)
    # The census measures every math string; the sample is a subset of the same measurements.
    items = chosen if args.sample_only else ranked
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        boxes = list(pool.map(lambda item: measure_one(item["text"], preamble), items))
    measured = {item["sha256"]: box for item, box in zip(items, boxes)}
    write_fixture(
        FIXTURE,
        "# Real TeX boxes for a stratified corpus sample (fm-tex-layout-oracle-bkbc).",
        [
            f"# Sample: up to {PER_CLASS} strings per construct class by",
            f"# occurrence, then the most frequent remaining strings, {len(chosen)} candidates;",
        ],
        chosen,
        measured,
    )
    if not args.sample_only:
        write_fixture(
            CENSUS,
            "# Real TeX boxes for every math-mode corpus string (fm-box-oracle-full-corpus-r11-lykz).",
            [
                "# Census: every math-mode string of the frozen G0-4 corpus, most frequent first,",
                f"# {len(ranked)} candidates; tex_corpus_boxes.v1.tsv is its stratified sample;",
            ],
            ranked,
            measured,
        )
    return 0


def write_fixture(path: pathlib.Path, title: str, scope: list[str], items: list[dict],
                  measured: dict[str, tuple[float, float, float] | None]) -> None:
    rows, failed = [], 0
    for item in items:
        box = measured[item["sha256"]]
        if box is None or box[0] <= 0.0 or box[1] + box[2] <= 0.0:
            failed += 1
            continue
        # "|" appears in no construct name ("\\," and "\\ " rule out "," and " ").
        classes = "|".join(sorted(item["constructs"])) or "(plain)"
        width, height, depth = box
        rows.append(f"{item['sha256']}\t{width:.5f}\t{height:.5f}\t{depth:.5f}\t{item['count']}\t{classes}")
    header = [
        title,
        "# Strings are private fixtures (plan §15.3): each row names its corpus string by",
        "# sha256(mode + NUL + string). Width, height and depth are TeX's \\wd, \\ht and \\dp",
        "# of the stripped string boxed in display style, in ems at 10 pt: braced like an align*",
        "# cell, or for multi-line strings as `aligned`[t] without its invisible struts and glue,",
        "# with amsmath's \\if@display set as align* sets it.",
        "# Preamble: the pinned Reference's default template (3b1b/manim 6199a00d); TeX Live",
        "# 2025 on the dev host.",
        *scope,
        f"# {failed} TeX could not typeset alone (fragments of multi-argument Tex calls) are",
        "# omitted. Regenerate only with scripts/capture_tex_corpus_boxes.py; never edit by hand.",
        "# sha256\twidth\theight\tdepth\tcount\tclasses (|-separated construct names)",
    ]
    path.write_text("\n".join(header + rows) + "\n", encoding="utf-8")
    print(f"{len(rows)} rows, {failed} omitted -> {path.relative_to(ROOT)}")


if __name__ == "__main__":
    sys.exit(main())
