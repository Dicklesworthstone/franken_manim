#!/usr/bin/env python3
"""Grammar-driven differential testing of fmd-math against real TeX boxes
(fm-tex-grammar-differential-r12-i0nh).

A seeded generator builds formulas from the constructs fmd-math supports,
beyond what the corpus uses. Real TeX boxes each one, as the box census does:
display style, the Reference preamble, braced like an align* cell, or for
multi-line formulas as a top-aligned `aligned` without its struts. fmd-math
boxes the same strings through the census's own path (the
`tex_box_batch` example: display style, the default macro pack). A formula
whose width, height or depth differ by more than TOLERANCE em is a mismatch.
It shrinks to the shortest of its pre-measured sub-formulas that still
mismatches.

The generated formulas are this script's own, not corpus text; they stay
under the gitignored frames/ directory with their seed. TeX never enters the
product: this is a dev-time oracle that needs a local `latex`.

Usage (RUN is a directory, conventionally frames/grammar/<name>):
  tex_grammar_differential.py generate --seeds 1-10 --count 200 --out RUN
  tex_grammar_differential.py tex RUN [--plant sup1] [--jobs 8]
  rch exec --job --result-dir RUN -- cargo run -p fmn-conformance \\
      --example tex_box_batch -- RUN/cases.tsv RUN/fmd_boxes.tsv
  tex_grammar_differential.py report RUN [--plant sup1]

`--plant sup1` is the oracle's own planted defect: TeX boxes the formulas with
cmsy10's sup1 (\\fontdimen13, the display-style superscript shift) 1% larger,
so a run that finds no mismatch under it is not testing anything.
"""
from __future__ import annotations

import argparse
import concurrent.futures
import dataclasses
import hashlib
import json
import pathlib
import random
import re
import subprocess
import sys
import tempfile

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import capture_tex_corpus_boxes as census  # noqa: E402

TOLERANCE = 1e-4  # em
BATCH = 150
BOX = re.compile(r"FMNBOX (\S+) ([0-9.]+)pt ([0-9.]+)pt ([0-9.]+)pt")
PLANTS = {
    # cmsy10's sup1, the superscript shift in display style (TeXbook
    # Appendix G rule 18a), 1% larger. TeX shares a font loaded twice at the
    # same size, so this reaches \textfont2.
    "sup1": "\\font\\fmnplant=cmsy10 \\fontdimen13\\fmnplant=1.01\\fontdimen13\\fmnplant\n",
}

# ---------------------------------------------------------------------------
# The grammar
# ---------------------------------------------------------------------------

LETTERS = list("abcdxyznmkABCPQ")
DIGITS = list("0123456789")
LIMITS = DIGITS + ["\\infty"]
GREEK = ["\\alpha", "\\beta", "\\gamma", "\\delta", "\\theta", "\\lambda", "\\pi", "\\sigma",
         "\\omega", "\\phi", "\\Gamma", "\\Delta", "\\Omega", "\\Sigma"]
BINS = ["+", "-", "\\times", "\\cdot", "\\pm"]
RELS = ["=", "<", ">", "\\le", "\\ge", "\\neq", "\\approx", "\\to", "\\in", "\\sim"]
PUNCT = [",", ";"]
BIGOPS = ["\\sum", "\\prod", "\\int", "\\oint", "\\bigcup", "\\bigcap"]
LIMOPS = ["\\lim", "\\max", "\\min", "\\sup"]
FUNCS = ["\\sin", "\\cos", "\\log", "\\exp", "\\tan", "\\ln"]
ACCENTS = ["\\hat", "\\bar", "\\vec", "\\tilde", "\\dot", "\\ddot", "\\dddot", "\\overline",
           "\\underline", "\\widehat"]
FRACS = ["\\frac", "\\dfrac", "\\tfrac", "\\binom", "\\dbinom", "\\tbinom"]
DELIMS = [("(", ")"), ("[", "]"), ("\\{", "\\}"), ("|", "|"), ("\\langle", "\\rangle"),
          (".", "|"), ("\\lfloor", "\\rfloor")]
MATRICES = ["pmatrix", "bmatrix", "vmatrix", "matrix"]
SPACES = ["\\,", "\\;", "\\quad", "\\!", "\\:"]
STYLES = ["\\textstyle", "\\scriptstyle", "\\scriptscriptstyle", "\\displaystyle"]
WORDS = ["if", "and", "for all", "max", "rand"]


@dataclasses.dataclass(frozen=True)
class Node:
    """A formula: its TeX, its construct, and the sub-formulas that stand alone."""
    tex: str
    kind: str
    parts: tuple["Node", ...] = ()


def atom(rng: random.Random) -> Node:
    pick = rng.random()
    if pick < 0.45:
        return Node(rng.choice(LETTERS), "letter")
    if pick < 0.65:
        return Node(rng.choice(DIGITS), "digit")
    return Node(rng.choice(GREEK), "greek")


def braced(node: Node) -> str:
    return "{" + node.tex + "}"


def operand(rng: random.Random, depth: int) -> Node:
    """A term: an atom, or a structure while depth remains."""
    if depth <= 0 or rng.random() < 0.35:
        return atom(rng)
    builders = [scripts, scripts, frac, sqrt, delimited, accent, func, bigop, limop, matrix,
                styled, text, operatorname]
    return rng.choice(builders)(rng, depth - 1)


def expression(rng: random.Random, depth: int, items: int | None = None) -> Node:
    """Operands joined by binary operators, relations, punctuation and spacing."""
    count = items if items is not None else rng.randint(1, 3)
    pieces: list[Node] = [operand(rng, depth)]
    for _ in range(count - 1):
        joint = rng.random()
        if joint < 0.45:
            pieces.append(Node(rng.choice(BINS), "bin"))
        elif joint < 0.75:
            pieces.append(Node(rng.choice(RELS), "rel"))
        elif joint < 0.85:
            pieces.append(Node(rng.choice(PUNCT), "punct"))
        else:
            pieces.append(Node(rng.choice(SPACES), "space"))
        pieces.append(operand(rng, depth))
    if len(pieces) == 1:
        return pieces[0]
    operands = tuple(piece for piece in pieces if piece.kind not in ("bin", "rel", "punct", "space"))
    # Shrink candidates: each operand alone, and the expression without its
    # last joint and operand.
    shorter = Node(" ".join(p.tex for p in pieces[:-2]), "seq", ()) if len(pieces) > 3 else None
    parts = operands + ((shorter,) if shorter else ())
    return Node(" ".join(p.tex for p in pieces), "seq", parts)


def scripts(rng: random.Random, depth: int) -> Node:
    base = atom(rng) if rng.random() < 0.8 else operand(rng, depth)
    sub = expression(rng, depth, rng.randint(1, 2)) if rng.random() < 0.6 else None
    sup = expression(rng, depth, rng.randint(1, 2)) if sub is None or rng.random() < 0.5 else None
    tex = base.tex if base.kind in ("letter", "digit", "greek") else braced(base)
    parts = [base]
    if sub is not None:
        tex += "_" + braced(sub)
        parts.append(sub)
    if sup is not None:
        tex += "^" + braced(sup)
        parts.append(sup)
    if sub is not None and sup is not None:
        parts.append(Node(base.tex + "_" + braced(sub), "scripts"))
        parts.append(Node(base.tex + "^" + braced(sup), "scripts"))
    return Node(tex, "scripts", tuple(parts))


def frac(rng: random.Random, depth: int) -> Node:
    command = rng.choice(FRACS)
    num, den = expression(rng, depth), expression(rng, depth)
    return Node(f"{command}{braced(num)}{braced(den)}", command[1:], (num, den))


def sqrt(rng: random.Random, depth: int) -> Node:
    body = expression(rng, depth)
    if rng.random() < 0.4:
        index = atom(rng)
        return Node(f"\\sqrt[{index.tex}]{braced(body)}", "sqrt-index",
                    (body, Node(f"\\sqrt{braced(body)}", "sqrt", (body,))))
    return Node(f"\\sqrt{braced(body)}", "sqrt", (body,))


def delimited(rng: random.Random, depth: int) -> Node:
    left, right = rng.choice(DELIMS)
    body = expression(rng, depth)
    return Node(f"\\left{left} {body.tex} \\right{right}", "left-right", (body,))


def accent(rng: random.Random, depth: int) -> Node:
    command = rng.choice(ACCENTS)
    body = atom(rng) if rng.random() < 0.7 else expression(rng, depth, 1)
    return Node(f"{command}{braced(body)}", command[1:], (body,))


def func(rng: random.Random, depth: int) -> Node:
    name = rng.choice(FUNCS)
    arg = operand(rng, depth)
    return Node(f"{name} {arg.tex}", "func", (arg,))


def bigop(rng: random.Random, depth: int) -> Node:
    op = rng.choice(BIGOPS)
    lower = Node(f"{rng.choice(LETTERS)}={rng.choice(DIGITS)}", "seq")
    upper = atom(rng)
    body = expression(rng, depth)
    head = f"{op}_{braced(lower)}^{braced(upper)}"
    return Node(f"{head} {body.tex}", "bigop", (body, Node(head, "bigop")))


def limop(rng: random.Random, depth: int) -> Node:
    op = rng.choice(LIMOPS)
    lower = Node(f"{rng.choice(LETTERS)} \\to {rng.choice(LIMITS)}", "seq")
    body = operand(rng, depth)
    head = f"{op}_{braced(lower)}"
    return Node(f"{head} {body.tex}", "limop", (body, Node(head, "limop")))


def matrix(rng: random.Random, depth: int) -> Node:
    env = rng.choice(MATRICES)
    rows, cols = rng.randint(1, 3), rng.randint(1, 3)
    cells = [[expression(rng, max(depth - 1, 0), 1) for _ in range(cols)] for _ in range(rows)]
    body = " \\\\ ".join(" & ".join(cell.tex for cell in row) for row in cells)
    return Node(f"\\begin{{{env}}} {body} \\end{{{env}}}", env,
                tuple(cell for row in cells for cell in row))


def styled(rng: random.Random, depth: int) -> Node:
    style = rng.choice(STYLES)
    body = expression(rng, depth)
    return Node(f"{{{style} {body.tex}}}", style[1:], (body,))


def text(rng: random.Random, depth: int) -> Node:
    del depth
    return Node(f"\\text{{{rng.choice(WORDS)}}}", "text")


def operatorname(rng: random.Random, depth: int) -> Node:
    word = rng.choice(["rank", "tr", "sgn", "Var"])
    arg = operand(rng, depth)
    return Node(f"\\operatorname{{{word}}} {arg.tex}", "operatorname", (arg,))


def formulas(seed: int, count: int) -> list[Node]:
    rng = random.Random(seed)
    return [expression(rng, rng.randint(1, 3), rng.randint(1, 4)) for _ in range(count)]


def subformulas(node: Node) -> list[Node]:
    """The node and every standalone sub-formula, depth first."""
    found = [node]
    for part in node.parts:
        found.extend(subformulas(part))
    return found


def case_id(tex: str) -> str:
    return hashlib.sha256(tex.encode("utf-8")).hexdigest()[:16]


# ---------------------------------------------------------------------------
# generate
# ---------------------------------------------------------------------------

def parse_seeds(text: str) -> list[int]:
    seeds: list[int] = []
    for chunk in text.split(","):
        low, _, high = chunk.partition("-")
        seeds.extend(range(int(low), int(high or low) + 1))
    return seeds


def generate(args: argparse.Namespace) -> int:
    run = pathlib.Path(args.out)
    run.mkdir(parents=True, exist_ok=True)
    cases: dict[str, str] = {}
    roots = []
    for seed in parse_seeds(args.seeds):
        for index, root in enumerate(formulas(seed, args.count)):
            subs = [sub for sub in subformulas(root) if sub.tex.strip()]
            for sub in subs:
                cases.setdefault(case_id(sub.tex), sub.tex)
            roots.append({
                "seed": seed, "index": index, "id": case_id(root.tex), "kind": root.kind,
                "candidates": [[case_id(sub.tex), sub.kind] for sub in subs],
            })
    (run / "cases.tsv").write_text(
        "".join(f"{key}\t{tex}\n" for key, tex in sorted(cases.items())), encoding="utf-8")
    (run / "roots.json").write_text(json.dumps({"seeds": args.seeds, "count": args.count,
                                                "roots": roots}, indent=1), encoding="utf-8")
    print(f"generated {len(roots)} formulas, {len(cases)} distinct cases with their "
          f"sub-formulas, in {run}")
    return 0


# ---------------------------------------------------------------------------
# tex
# ---------------------------------------------------------------------------

def tex_batch(batch: list[tuple[str, str]], preamble: str, multiline: bool,
              plant: str) -> dict[str, tuple[float, float, float] | None]:
    """Box a batch in one LaTeX run; a formula whose segment logs an error is None."""
    unpad = ("\\makeatletter\\def\\strut@{}\\makeatother\\renewcommand{\\minalignsep}{0pt}\n"
             if multiline else "")
    body = []
    for key, tex in batch:
        inner = (f"\\begin{{aligned}}[t]\n{tex}\n\\end{{aligned}}" if multiline
                 else f"{{{tex}\n}}")
        body.append(f"\\typeout{{FMNBEGIN {key}}}\n\\sbox\\fmnbox{{$\\displaystyle\n{inner}\n$}}\n"
                    f"\\typeout{{FMNBOX {key} \\the\\wd\\fmnbox\\space\\the\\ht\\fmnbox"
                    f"\\space\\the\\dp\\fmnbox}}\n")
    document = ("\\documentclass{article}\n" + preamble + "\n" + unpad + plant
                + "\\newsavebox\\fmnbox\n\\begin{document}\n"
                "\\makeatletter\\@displaytrue\\makeatother\n" + "".join(body)
                + "\\end{document}\n")
    with tempfile.TemporaryDirectory(prefix="tex-grammar-") as work:
        (pathlib.Path(work) / "f.tex").write_text(document, encoding="utf-8")
        try:
            subprocess.run(["latex", "-interaction=nonstopmode", "f.tex"], cwd=work,
                           capture_output=True, timeout=600, check=False)
        except subprocess.TimeoutExpired:
            return {key: None for key, _ in batch}
        log = (pathlib.Path(work) / "f.log").read_text(encoding="latin-1", errors="replace")
    result: dict[str, tuple[float, float, float] | None] = {key: None for key, _ in batch}
    for segment in log.split("FMNBEGIN ")[1:]:
        key = segment.split(None, 1)[0]
        match = BOX.search(segment)
        if match is None or "\n! " in segment.split("FMNBOX", 1)[0] or match.group(1) != key:
            continue
        result[key] = tuple(float(v) / 10.0 for v in match.groups()[1:])
    return result


def tex(args: argparse.Namespace) -> int:
    run = pathlib.Path(args.run)
    cases = [tuple(line.split("\t", 1)) for line in
             (run / "cases.tsv").read_text(encoding="utf-8").splitlines() if line]
    # The census's math preamble: the Reference's without tipa (Appendix C-41).
    preamble = census.ruled_math_preamble()
    plant = PLANTS[args.plant] if args.plant else ""
    groups = {False: [], True: []}
    for key, text in cases:
        groups["\\\\" in text or "&" in text].append((key, text))
    # Single-line formulas share a document. A TeX error inside an alignment
    # derails the rest of its document, so multi-line formulas get one each,
    # and so does every formula a shared document could not box: an error is
    # then the formula's own.
    texts = dict(cases)
    boxes: dict[str, tuple[float, float, float] | None] = {}
    with concurrent.futures.ThreadPoolExecutor(args.jobs) as pool:
        jobs = [pool.submit(tex_batch, groups[False][start:start + BATCH], preamble, False, plant)
                for start in range(0, len(groups[False]), BATCH)]
        jobs += [pool.submit(tex_batch, [item], preamble, True, plant) for item in groups[True]]
        for job in jobs:
            boxes.update(job.result())
        retry = [key for key, box in boxes.items() if box is None and key not in dict(groups[True])]
        for result in pool.map(lambda key: tex_batch([(key, texts[key])], preamble, False, plant),
                               retry):
            boxes.update(result)
    # The batch must box as the census's one-document-per-string capture does:
    # check a deterministic sample against measure_one (unplanted only).
    if not args.plant:
        sample = sorted(cases)[:: max(1, len(cases) // args.verify)]
        for key, text in sample:
            alone = census.measure_one(text, preamble)
            batched = boxes.get(key)
            if (alone is None) != (batched is None) or (
                    alone is not None and max(abs(a - b) for a, b in zip(alone, batched)) > 1e-6):
                raise SystemExit(f"batched TeX box {batched} differs from the census capture "
                                 f"{alone} for {text!r}")
        print(f"checked {len(sample)} batched boxes against the census capture")
    name = "tex_boxes" + (f".{args.plant}" if args.plant else "") + ".tsv"
    (run / name).write_text("".join(
        f"{key}\t" + ("\t".join(f"{v:.6f}" for v in box) if box else "ERR") + "\n"
        for key, box in sorted(boxes.items())), encoding="utf-8")
    refused = sum(box is None for box in boxes.values())
    print(f"TeX boxed {len(boxes) - refused} of {len(boxes)} cases ({refused} TeX errors) "
          f"into {run / name}")
    return 0


# ---------------------------------------------------------------------------
# report
# ---------------------------------------------------------------------------

def read_boxes(path: pathlib.Path) -> dict[str, tuple[float, float, float] | str]:
    boxes: dict[str, tuple[float, float, float] | str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        fields = line.split("\t")
        if len(fields) >= 4 and fields[1] not in ("ERR", "PANIC"):
            boxes[fields[0]] = tuple(float(v) for v in fields[1:4])
        else:
            boxes[fields[0]] = fields[1] + (": " + fields[2] if len(fields) > 2 else "")
    return boxes


def report(args: argparse.Namespace) -> int:
    run = pathlib.Path(args.run)
    roots = json.loads((run / "roots.json").read_text(encoding="utf-8"))
    texts = dict(line.split("\t", 1) for line in
                 (run / "cases.tsv").read_text(encoding="utf-8").splitlines() if line)
    tex_boxes = read_boxes(run / ("tex_boxes" + (f".{args.plant}" if args.plant else "") + ".tsv"))
    fmd_boxes = read_boxes(run / "fmd_boxes.tsv")

    def verdict(key: str) -> tuple[str, tuple[float, float, float] | None]:
        reference, laid = tex_boxes.get(key), fmd_boxes.get(key)
        if not isinstance(reference, tuple):
            return "tex-error", None
        if isinstance(laid, str) and laid.startswith("PANIC"):
            return "panic", None
        if not isinstance(laid, tuple):
            return "refused", None
        delta = tuple(b - a for a, b in zip(reference, laid))
        return ("mismatch" if max(map(abs, delta)) > TOLERANCE else "match"), delta

    counts: dict[str, int] = {}
    records = []
    classes: dict[tuple[str, str], dict] = {}
    for root in roots["roots"]:
        state, delta = verdict(root["id"])
        counts[state] = counts.get(state, 0) + 1
        if state not in ("mismatch", "refused", "panic"):
            continue
        # Shrink: the shortest pre-measured sub-formula in the same state.
        candidates = [(len(texts[key]), texts[key], key, kind) for key, kind in root["candidates"]
                      if verdict(key)[0] == state]
        _, minimal, key, kind = min(candidates)
        minimal_delta = verdict(key)[1]
        dims = "".join(name for name, value in zip("whd", minimal_delta or (0, 0, 0))
                       if abs(value) > TOLERANCE) if state == "mismatch" else state
        record = {"seed": root["seed"], "index": root["index"], "state": state,
                  "formula": texts[root["id"]], "minimal": minimal, "minimal_kind": kind,
                  "dims": dims, "tex": tex_boxes.get(key), "fmd": fmd_boxes.get(key)}
        records.append(record)
        cls = classes.setdefault((kind, dims), {"count": 0, "example": record})
        cls["count"] += 1
        if len(minimal) < len(cls["example"]["minimal"]):
            cls["example"] = record
    # The plant's own effect: cases that match against unplanted TeX and
    # mismatch against planted TeX. Zero means the run cannot see the defect.
    detected = None
    if args.plant:
        unplanted = read_boxes(run / "tex_boxes.tsv")
        detected = 0
        for key, laid in fmd_boxes.items():
            reference = unplanted.get(key)
            if not (isinstance(laid, tuple) and isinstance(reference, tuple)):
                continue
            was = max(abs(b - a) for a, b in zip(reference, laid)) > TOLERANCE
            if not was and verdict(key)[0] == "mismatch":
                detected += 1
    name = "report" + (f".{args.plant}" if args.plant else "")
    with (run / f"{name}.ndjson").open("w", encoding="utf-8") as out:
        for record in records:
            out.write(json.dumps(record) + "\n")
    summary = {"schema": "fmn.tex-grammar-differential.v1", "seeds": roots["seeds"],
               "formulas": len(roots["roots"]), "cases": len(fmd_boxes), "plant": args.plant or None,
               "tolerance_em": TOLERANCE, **counts}
    if detected is not None:
        summary["plant_detected_cases"] = detected
    print(json.dumps(summary))
    if detected == 0:
        print("the planted defect moved no case: this run cannot see it", file=sys.stderr)
        return 1
    for (kind, dims), cls in sorted(classes.items(), key=lambda item: -item[1]["count"]):
        example = cls["example"]
        print(f"{cls['count']:5d}  {kind:14s} {dims:9s} {example['minimal']!r}  "
              f"tex={example['tex']} fmd={example['fmd']}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)
    gen = commands.add_parser("generate")
    gen.add_argument("--seeds", default="1-10")
    gen.add_argument("--count", type=int, default=200)
    gen.add_argument("--out", required=True)
    tx = commands.add_parser("tex")
    tx.add_argument("run")
    tx.add_argument("--plant", choices=sorted(PLANTS))
    tx.add_argument("--jobs", type=int, default=8)
    tx.add_argument("--verify", type=int, default=40, help="sample size checked against measure_one")
    rep = commands.add_parser("report")
    rep.add_argument("run")
    rep.add_argument("--plant", choices=sorted(PLANTS))
    args = parser.parse_args()
    return {"generate": generate, "tex": tex, "report": report}[args.command](args)


if __name__ == "__main__":
    raise SystemExit(main())
