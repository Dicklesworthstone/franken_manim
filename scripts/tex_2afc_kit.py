#!/usr/bin/env python3
"""Blind 2AFC TeX review kit (fm-tex-2afc-kit-b9k2): which of two renders is LaTeX?

"Side-by-side indistinguishable at a glance from LaTeX" is a claim about perception, so the kit
turns it into a test a reviewer can pass or fail in about 20 minutes. Each pair shows one corpus
formula twice. One render is LaTeX, captured from the pinned Reference. The other is fmd-math.
The reviewer picks the LaTeX side; scripts/score_tex_2afc.py scores the answers.

Both sides go through the same rasterizer: the portal's certified engine (`--reproducible`), white
on #333333 at the same font size, cropped to one padded box per pair. The LaTeX side is the
Reference's own `Tex(formula, font_size=F)`, captured as point arrays (its TeX -> dvisvgm geometry,
at its own scale) through a private TeX cache. The pinned Reference's renderer would otherwise
leave a rasterization tell, and the test is about typesetting.

Steps (each runs under the interpreter that owns it):
  sample    host Python: a seeded, occurrence-weighted draw of PER_STRATUM formulas per stratum
            from the private corpus, with backups in draw order
  capture   the Reference venv (xvfb): each candidate's Tex geometry, or why LaTeX refused it
  render    the portal venv: both sides of each pair as certified PNGs, then the common crop, the
            page, the key and the manifest

Usage (run under the portal venv's Python, which has NumPy and Pillow):
  PORTAL_PY tex_2afc_kit.py build --corpus corpus/tex_corpus.jsonl --out DIR --seed N \\
      --portal-python PORTAL_PY --reference-python REF_PY [--reference-root scripts/manim_ref]
      [--control latex-vs-latex|latex-vs-plex] [--per-stratum 20] [--backups 40]
  PORTAL_PY tex_2afc_kit.py simulate --kit DIR --reviewer latex-match|random [--seed N]
  score_tex_2afc.py ANSWERS.json DIR/key.json

The corpus is private (plan §15.3) and its formulas are CC BY-NC-SA: the page and its images are
written under --out, outside the repository. The manifest and key carry formula digests only.
"""
from __future__ import annotations

import argparse
import base64
import concurrent.futures
import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
SCHEMA = "fmn.tex-2afc-kit"
VERSION = 1
PER_STRATUM = 20
BACKUPS = 40
FONT_SIZE = 60
RESOLUTION = (1920, 1080)
BACKGROUND = (0x33, 0x33, 0x33)
PAD = 24
MAX_CHARS = 120

GREEK = ("alpha", "beta", "gamma", "delta", "epsilon", "varepsilon", "zeta", "eta", "theta", "vartheta",
         "iota", "kappa", "lambda", "mu", "nu", "xi", "pi", "varpi", "rho", "varrho", "sigma", "varsigma",
         "tau", "upsilon", "phi", "varphi", "chi", "psi", "omega", "Gamma", "Delta", "Theta", "Lambda",
         "Xi", "Pi", "Sigma", "Upsilon", "Phi", "Psi", "Omega")
RELATIONS = ("le", "leq", "ge", "geq", "ne", "neq", "approx", "sim", "simeq", "equiv", "cong", "propto",
             "to", "rightarrow", "Rightarrow", "leftarrow", "Leftarrow", "leftrightarrow", "Leftrightarrow",
             "mapsto", "implies", "iff", "in", "notin", "subset", "subseteq", "supset", "cup", "cap",
             "cdot", "times", "div", "pm", "mp", "ll", "gg", "perp", "parallel", "circ", "otimes", "oplus")
# The five strata, in draw order. A formula drawn for one stratum is not drawn again for another.
STRATA = (
    ("fractions-radicals", lambda c, t: bool({r"\frac", r"\dfrac", r"\tfrac", r"\cfrac", r"\over", r"\sqrt"} & c)),
    ("scripts-limits", lambda c, t: bool({"script:sub", "script:sup", r"\lim", r"\sum", r"\prod", r"\int",
                                          r"\oint", r"\max", r"\min", r"\sup", r"\inf", r"\limsup",
                                          r"\liminf"} & c)),
    ("delimiters-matrices", lambda c, t: bool({r"\left", r"\right", r"\big", r"\Big", r"\bigg", r"\Bigg",
                                               r"\langle", r"\rangle", r"\lfloor", r"\rfloor", r"\lceil",
                                               r"\rceil", r"\binom"} & c)
                                         or any(x.startswith("env:") for x in c)),
    ("greek-calligraphic", lambda c, t: bool({"\\" + g for g in GREEK} & c)
                                        or bool({r"\mathcal", r"\mathbb", r"\mathfrak", r"\mathscr"} & c)),
    ("relations-operators", lambda c, t: bool({"\\" + r for r in RELATIONS} & c) or bool(re.search(r"[=<>+]", t))),
)


def rng_for(seed: int, name: str):
    """The one RNG (PCG64DXSM) on a named substream: numpy's SeedSequence keyed by the name's
    SHA-256, so each stream is reproducible from (seed, name) alone."""
    import numpy as np

    key = int.from_bytes(hashlib.sha256(name.encode("utf-8")).digest()[:8], "little")
    return np.random.Generator(np.random.PCG64DXSM(np.random.SeedSequence(seed, spawn_key=(key,))))


def assign(strata: list[str], seed: int) -> tuple[list[int], list[bool]]:
    """Presentation order (a permutation of the pairs) and, per pair, whether the LaTeX render
    sits on the left. Each comes from its own named substream of the seed.

    Sides are balanced exactly within every stratum (half left, the odd one out on the right),
    then shuffled within it, so a reviewer who always picks one side scores chance overall and
    in each stratum. Coin flips let such a reviewer fail with no information: one seed put LaTeX
    on the left 59 times of 100, and a balanced-overall draw put it left 16 of 20 in one stratum,
    the stratum rule's limit."""
    order = [int(i) for i in rng_for(seed, "tex-2afc/order").permutation(len(strata))]
    sides = [False] * len(strata)
    for name in sorted(set(strata)):
        members = [i for i, s in enumerate(strata) if s == name]
        balanced = [True] * (len(members) // 2) + [False] * (len(members) - len(members) // 2)
        for i, side in zip(members, rng_for(seed, f"tex-2afc/sides/{name}").permutation(balanced)):
            sides[i] = bool(side)
    return order, sides


def eligible(record) -> bool:
    text = record["text"].strip()
    return (record["mode"] == "math" and bool(text) and len(text) <= MAX_CHARS
            and "\\\\" not in text and "&" not in text)


def sample(corpus: pathlib.Path, seed: int, per_stratum: int = PER_STRATUM, backups: int = BACKUPS):
    """Per stratum, an occurrence-weighted draw without replacement (Efraimidis-Spirakis keys
    u ** (1 / count) from the stratum's own substream), longest key first. The first
    `per_stratum` are the pairs; the next `backups` stand in, in order, for any LaTeX or fmd-math
    refuses. Ties break by digest, so the draw depends only on (seed, corpus)."""
    records = [json.loads(line) for line in corpus.open(encoding="utf-8")]
    pool = [r for r in records if eligible(r)]
    pool.sort(key=lambda r: r["sha256"])
    taken: set[str] = set()
    draws = {}
    for name, test in STRATA:
        members = [r for r in pool if r["sha256"] not in taken and test(set(r["constructs"]), r["text"])]
        rng = rng_for(seed, f"tex-2afc/{name}")
        scored = [(float(u) ** (1.0 / r["count"]), r["sha256"], r)
                  for u, r in zip(rng.random(len(members)), members)]
        scored.sort(key=lambda item: (-item[0], item[1]))
        chosen = [r for _, _, r in scored[: per_stratum + backups]]
        taken.update(r["sha256"] for r in chosen)
        draws[name] = [{"id": r["sha256"], "text": r["text"].strip(), "count": r["count"]} for r in chosen]
    return draws


# --------------------------------------------------------------------------------- capture


def capture(candidates_path: pathlib.Path, out_path: pathlib.Path, config: pathlib.Path) -> int:
    """Under the Reference venv: each candidate's Tex(formula, font_size=FONT_SIZE) point arrays."""
    sys.argv = ["tex-2afc-capture", "--config_file", str(config)]
    from manimlib import Tex  # noqa: E402 - the Reference parses argv at import

    results = {}
    for item in json.loads(candidates_path.read_text(encoding="utf-8")):
        try:
            mob = Tex(item["text"], font_size=FONT_SIZE)
            parts = [m.get_points().round(6).tolist() for m in mob.family_members_with_points()]
            results[item["id"]] = {"parts": parts}
        except Exception as error:  # noqa: BLE001 - a refusal is recorded, not fatal
            results[item["id"]] = {"error": f"{type(error).__name__}: {str(error)[:200]}"}
    out_path.write_text(json.dumps(results), encoding="utf-8")
    return 0


# --------------------------------------------------------------------------------- render

SCENE_MODULE = '''"""Generated by tex_2afc_kit.py: one side of one 2AFC pair per scene invocation."""
import json
import os

import numpy as np
from manimlib import *

PAIR = json.loads(open(os.environ["FMN_2AFC_PAIR"], encoding="utf-8").read())


class Side(Scene):
    camera_config = dict(background_color="#333333")

    def construct(self):
        side = os.environ["FMN_2AFC_SIDE"]
        if side == "fmd":
            mob = Tex(PAIR["text"], font_size=PAIR["font_size"])
        elif side == "plex":
            mob = Text(PAIR["text"], font="IBM Plex Sans", font_size=PAIR["font_size"])
        else:
            mob = VGroup(*(VMobject().set_points(np.array(part, dtype=float)) for part in PAIR["parts"]))
        mob.set_fill(WHITE, 1).set_stroke(width=0)
        mob.move_to(ORIGIN)
        self.add(mob)
'''


def run_side(portal_python, work, pair_file, side, png):
    env = dict(os.environ, FMN_2AFC_PAIR=str(pair_file), FMN_2AFC_SIDE=side,
               OMP_NUM_THREADS="1", OPENBLAS_NUM_THREADS="1")
    argv = [portal_python, "-m", "fmn_python", str(work / "kit_scenes.py"), "Side", "-s", "--reproducible",
            "--format", "png", "--resolution", f"{RESOLUTION[0]}x{RESOLUTION[1]}", "--threads", "1",
            "--output", str(png)]
    result = subprocess.run(argv, cwd=work, env=env, capture_output=True, text=True, timeout=300)
    return result.returncode, (result.stderr or result.stdout).strip().splitlines()[-1:] if result.returncode else []


def render_identity(png) -> tuple[str, str]:
    """(build_id, mode) from the portal's provenance manifest beside a rendered PNG."""
    fields = {}
    for line in (pathlib.Path(f"{png}.manifest") / "manifest.txt").read_text(encoding="utf-8").splitlines():
        key, sep, value = line.partition(" = ")
        if sep:
            fields[key.strip()] = value.strip().strip('"')
    return fields.get("build_id", "unknown"), fields.get("mode", "unknown")


def ink_box(pixels):
    import numpy as np

    mask = np.any(np.abs(pixels[:, :, :3].astype(int) - np.array(BACKGROUND)) > 8, axis=2)
    rows, cols = np.nonzero(mask)
    if rows.size == 0:
        return None
    return rows.min(), rows.max(), cols.min(), cols.max()


def crop_pair(left_png, right_png):
    """Both sides cropped to the union of their ink boxes plus PAD, so size gives nothing away."""
    import numpy as np
    from PIL import Image

    images = [np.asarray(Image.open(p).convert("RGB")) for p in (left_png, right_png)]
    boxes = [ink_box(img) for img in images]
    if any(b is None for b in boxes):
        return None
    top = max(0, min(b[0] for b in boxes) - PAD)
    bottom = min(images[0].shape[0], max(b[1] for b in boxes) + PAD + 1)
    left = max(0, min(b[2] for b in boxes) - PAD)
    right = min(images[0].shape[1], max(b[3] for b in boxes) + PAD + 1)
    return [img[top:bottom, left:right] for img in images]


def png_data_uri(array) -> str:
    import io

    from PIL import Image

    buffer = io.BytesIO()
    Image.fromarray(array).save(buffer, format="PNG", optimize=False)
    return "data:image/png;base64," + base64.b64encode(buffer.getvalue()).decode("ascii")


PAGE = """<!doctype html><meta charset="utf-8"><title>TeX 2AFC review</title>
<style>
body{{background:#1e1e1e;color:#ddd;font:15px system-ui,sans-serif;margin:16px}}
.pair{{border-top:1px solid #444;padding:12px 0}} .sides{{display:flex;gap:16px;flex-wrap:wrap}}
.side{{background:#333;padding:4px}} .side img{{max-width:100%;display:block}}
button{{margin:4px;padding:6px 14px}} button.chosen{{background:#4a7;color:#fff}}
.note{{width:22em}} #status{{position:sticky;top:0;background:#1e1e1e;padding:6px 0}}
</style>
<h1>Which render is LaTeX?</h1>
<p>{count} pairs. For each, pick the side you believe is LaTeX. Tick "visibly wrong" if either
side has a typesetting error, and say what in the note. Then download your answers. Nothing here
reveals the correct side; the key is a separate file.</p>
<div id="status"></div>
{pairs}
<p><button id="download">Download answers (JSON)</button></p>
<script>
const KIT = {kit_json};
const answers = {{}};
function status() {{
  const n = Object.keys(answers).filter(k => answers[k].choice).length;
  document.getElementById("status").textContent = n + " / " + KIT.pairs.length + " answered";
}}
document.querySelectorAll(".pair").forEach(div => {{
  const id = div.dataset.pair;
  answers[id] = {{choice: null, visibly_wrong: false, note: ""}};
  div.querySelectorAll("button[data-choice]").forEach(b => b.onclick = () => {{
    answers[id].choice = b.dataset.choice;
    div.querySelectorAll("button[data-choice]").forEach(x => x.classList.toggle("chosen", x === b));
    status();
  }});
  div.querySelector("input[type=checkbox]").onchange = e => answers[id].visibly_wrong = e.target.checked;
  div.querySelector("input.note").oninput = e => answers[id].note = e.target.value;
}});
document.getElementById("download").onclick = () => {{
  const blob = new Blob([JSON.stringify({{schema: "fmn.tex-2afc-answers", version: 1,
    kit: KIT.kit_id, answers}}, null, 1)], {{type: "application/json"}});
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = "tex_2afc_answers_" + KIT.kit_id + ".json";
  a.click();
}};
status();
</script>
"""

PAIR_HTML = """<div class="pair" data-pair="{pid}"><div>Pair {number}</div><div class="sides">
<div class="side"><img alt="left" src="{left}"><button data-choice="left">Left is LaTeX</button></div>
<div class="side"><img alt="right" src="{right}"><button data-choice="right">Right is LaTeX</button></div>
</div><label><input type="checkbox"> visibly wrong</label> <input class="note" placeholder="note"></div>"""


def build(args) -> int:
    out = args.out.resolve()
    if out.exists() and any(out.iterdir()):
        sys.exit(f"{out} is not empty; a kit is never overwritten")
    out.mkdir(parents=True, exist_ok=True)
    draws = sample(args.corpus, args.seed, args.per_stratum, args.backups)
    candidates = [dict(item, stratum=name) for name, items in draws.items() for item in items]
    (out / "candidates.json").write_text(json.dumps(candidates), encoding="utf-8")

    # LaTeX geometry from the Reference, through a private TeX cache (Appendix C-22).
    cache = out / "reference_tex"
    (cache / "latex").mkdir(parents=True)
    (cache / "svg").mkdir()
    config = cache / "config.yml"
    config.write_text(json.dumps({"directories": {"cache": str(cache / "svg"),
                                                  "subdirs": {"latex_cache": str(cache / "latex")}}}))
    env = {"PATH": "/usr/bin:/bin", "HOME": os.environ.get("HOME", "/tmp"), "PYTHONPATH": str(args.reference_root),
           "OMP_NUM_THREADS": "1"}
    subprocess.run(["xvfb-run", "-a", "-s", "-screen 0 640x480x24", args.reference_python, str(pathlib.Path(__file__).resolve()),
                    "capture", str(out / "candidates.json"), str(out / "latex_geometry.json"), str(config)],
                   cwd=out, env=env, check=True, timeout=3600)
    geometry = json.loads((out / "latex_geometry.json").read_text(encoding="utf-8"))

    # Render both sides of every candidate in stratum order until each stratum has its pairs.
    (out / "kit_scenes.py").write_text(SCENE_MODULE, encoding="utf-8")
    other = {"latex-vs-latex": "latex", "latex-vs-plex": "plex"}.get(args.control, "fmd")
    jobs = []
    for item in candidates:
        if "parts" not in geometry.get(item["id"], {}):
            continue
        work = out / "pairs" / item["id"][:16]
        work.mkdir(parents=True, exist_ok=True)
        pair_file = work / "pair.json"
        pair_file.write_text(json.dumps({"text": item["text"], "font_size": FONT_SIZE,
                                         "parts": geometry[item["id"]]["parts"]}), encoding="utf-8")
        jobs.append((item, work, pair_file))
    rendered = {}

    def render(job):
        item, work, pair_file = job
        sides = {}
        # The LaTeX-vs-LaTeX control shows one LaTeX render on both sides.
        for side in dict.fromkeys(("latex", other)):
            code, tail = run_side(args.portal_python, out, pair_file, side, work / f"{side}.png")
            if code != 0:
                return item["id"], {"error": f"{side}: exit {code}: {' '.join(tail)[:200]}"}
            sides[side] = work / f"{side}.png"
        return item["id"], {"latex": sides["latex"], "other": sides[other]}

    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for pid, result in pool.map(render, jobs):
            rendered[pid] = result

    pairs, refused = [], []
    for name, _ in STRATA:
        kept = 0
        for item in (c for c in candidates if c["stratum"] == name):
            if kept == args.per_stratum:
                break
            reason = geometry.get(item["id"], {}).get("error") or rendered.get(item["id"], {}).get("error")
            crops = None if reason else crop_pair(rendered[item["id"]]["latex"], rendered[item["id"]]["other"])
            if crops is None:
                refused.append({"id": item["id"], "stratum": name, "reason": reason or "no ink in a render"})
                continue
            pairs.append((item, crops))
            kept += 1
        if kept < args.per_stratum:
            sys.exit(f"stratum {name}: only {kept} usable pairs; raise --backups")

    order, sides = assign([item["stratum"] for item, _ in pairs], args.seed)
    kit_id = hashlib.sha256(json.dumps([args.seed, [item["id"] for item, _ in pairs], args.control or ""]).encode()).hexdigest()[:16]
    key, blocks = [], []
    for number, index in enumerate(order, 1):
        item, (latex_img, other_img) = pairs[index]
        latex_left = sides[index]
        left, right = (latex_img, other_img) if latex_left else (other_img, latex_img)
        pid = f"p{number:03d}"
        blocks.append(PAIR_HTML.format(pid=pid, number=number, left=png_data_uri(left), right=png_data_uri(right)))
        key.append({"pair": pid, "latex_side": "left" if latex_left else "right", "formula": item["id"],
                    "stratum": item["stratum"]})
    renders = {render_identity(png) for item, _ in pairs for png in rendered[item["id"]].values()}
    if any(mode != "certified" for _, mode in renders) or len(renders) != 1:
        sys.exit(f"renders are not all certified from one build: {sorted(renders)}")
    engines = {"reference": "3b1b/manim@6199a00d4c1b1127ebe45cb629c3f22538b10e13",
               "portal": next(iter(renders))[0], "portal_mode": next(iter(renders))[1]}
    manifest = {"schema": SCHEMA, "version": VERSION, "kit_id": kit_id, "seed": args.seed,
                "control": args.control, "font_size": FONT_SIZE, "resolution": list(RESOLUTION),
                "per_stratum": args.per_stratum, "backups": args.backups, "strata": [name for name, _ in STRATA], "engines": engines,
                "rng": "numpy PCG64DXSM, SeedSequence(seed, spawn_key=(sha256(name)[:8] LE,)), names tex-2afc/<stratum>|sides|order",
                "corpus_sha256": hashlib.sha256(args.corpus.read_bytes()).hexdigest(),
                "pairs": [{"pair": k["pair"], "formula": k["formula"], "stratum": k["stratum"]} for k in key],
                "refused": refused}
    (out / "key.json").write_text(json.dumps({"schema": "fmn.tex-2afc-key", "version": 1, "kit": kit_id,
                                             "pairs": key}, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    (out / "manifest.json").write_text(json.dumps(manifest, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    kit_json = json.dumps({"kit_id": kit_id, "pairs": [k["pair"] for k in key]})
    (out / "review.html").write_text(PAGE.format(count=len(blocks), pairs="\n".join(blocks), kit_json=kit_json),
                                     encoding="utf-8")
    print(json.dumps({"kit": kit_id, "pairs": len(blocks), "refused": len(refused), "out": str(out)}))
    return 0


PAIR_IMAGES = re.compile(r'data-pair="(p\d+)".*?alt="left" src="data:image/png;base64,([^"]+)"'
                         r'.*?alt="right" src="data:image/png;base64,([^"]+)"', re.S)


def simulate(kit: pathlib.Path, reviewer: str, seed: int) -> dict:
    """A simulated session that never reads the key, for the planted controls.

    `latex-match` looks at the page and picks the side pixel-identical to the pair's own LaTeX
    render (found through the public manifest's pair -> formula list), and the left side when
    both are. It is an oracle on images, not a model of perception: against LaTeX-vs-LaTeX it can
    only guess, and against any other kit it is always right. That makes it a check of the
    pipeline (sides, key and scorer), which is what the planted controls are. `random` picks a
    side from its own substream of the seed."""
    import io

    import numpy as np
    from PIL import Image

    manifest = json.loads((kit / "manifest.json").read_text(encoding="utf-8"))
    formula = {p["pair"]: p["formula"] for p in manifest["pairs"]}
    other = {"latex-vs-latex": "latex", "latex-vs-plex": "plex"}.get(manifest["control"], "fmd")
    rng = rng_for(seed, "tex-2afc/simulated-reviewer")
    answers = {}
    for pid, left, right in PAIR_IMAGES.findall((kit / "review.html").read_text(encoding="utf-8")):
        if reviewer == "random":
            choice = "left" if rng.integers(0, 2) else "right"
        else:
            work = kit / "pairs" / formula[pid][:16]
            latex = crop_pair(work / "latex.png", work / f"{other}.png")[0]
            shown = [np.asarray(Image.open(io.BytesIO(base64.b64decode(data))).convert("RGB")) for data in (left, right)]
            choice = "left" if np.array_equal(shown[0], latex) or not np.array_equal(shown[1], latex) else "right"
        answers[pid] = {"choice": choice, "visibly_wrong": False, "note": ""}
    return {"schema": "fmn.tex-2afc-answers", "version": 1, "kit": manifest["kit_id"], "answers": answers,
            "simulated_reviewer": reviewer}


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    b = sub.add_parser("build")
    b.add_argument("--corpus", type=pathlib.Path, required=True)
    b.add_argument("--out", type=pathlib.Path, required=True)
    b.add_argument("--seed", type=int, required=True)
    b.add_argument("--portal-python", required=True)
    b.add_argument("--reference-python", required=True)
    b.add_argument("--reference-root", type=pathlib.Path, default=ROOT / "scripts" / "manim_ref")
    b.add_argument("--control", choices=("latex-vs-latex", "latex-vs-plex"))
    b.add_argument("--jobs", type=int, default=8)
    b.add_argument("--per-stratum", type=int, default=PER_STRATUM)
    b.add_argument("--backups", type=int, default=BACKUPS)
    c = sub.add_parser("capture")
    c.add_argument("candidates", type=pathlib.Path)
    c.add_argument("out", type=pathlib.Path)
    c.add_argument("config", type=pathlib.Path)
    s = sub.add_parser("sample")
    s.add_argument("--corpus", type=pathlib.Path, required=True)
    s.add_argument("--seed", type=int, required=True)
    m = sub.add_parser("simulate", help="answer a kit with a simulated reviewer (planted controls)")
    m.add_argument("--kit", type=pathlib.Path, required=True)
    m.add_argument("--reviewer", choices=("latex-match", "random"), required=True)
    m.add_argument("--seed", type=int, default=0)
    args = parser.parse_args(argv)
    if args.command == "capture":
        return capture(args.candidates, args.out, args.config)
    if args.command == "simulate":
        print(json.dumps(simulate(args.kit, args.reviewer, args.seed), indent=1, sort_keys=True))
        return 0
    if args.command == "sample":
        draws = sample(args.corpus, args.seed)
        print(json.dumps({name: [d["id"][:16] for d in items[:PER_STRATUM]] for name, items in draws.items()}, indent=1))
        return 0
    return build(args)


if __name__ == "__main__":
    sys.exit(main())
