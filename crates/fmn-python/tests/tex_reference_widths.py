"""The Reference width oracle through the installed wheel (fm-tex-metrics-glyphs-ru72).

Each row of crates/fmn-conformance/fixtures/tex_reference_widths.v1.tsv is the pinned Reference's
own Tex(source) width at font_size 48 (scripts/capture_tex_reference_widths.py). The installed
portal's Tex must hold it within 3% for a plain row, or 6% for a row TeX set partly outside
Computer Modern. The exceptions are the native oracle's, in
crates/fmn-conformance/tests/tex_reference_widths.rs, which also prints fmd-math's per-glyph
layout for a failing row:
- Appendix C-27's tipa rows are excluded, and so are traced rows, whose Reference ink is the capture
  host's bitmap trace of its EC text fonts (BN-05).
- A row an open bead owns must still be out of tolerance.

Corpus rows are named only by digest, so they need the private corpus (FMN_TEX_CORPUS, or
corpus/tex_corpus.jsonl). Without it only the authored rows run.
"""
import hashlib
import json
import os
from pathlib import Path

import manimlib as m

ROOT = Path(__file__).resolve().parents[3]
FIXTURE = ROOT / "crates" / "fmn-conformance" / "fixtures" / "tex_reference_widths.v1.tsv"
NATIVE = ROOT / "crates" / "fmn-conformance" / "tests" / "tex_reference_widths.rs"
TOLERANCE = {"plain": 0.03, "fallback": 0.06}


def owned_rows():
    """The native oracle's OWNED table, read from its source so the two front doors share it."""
    text = NATIVE.read_text(encoding="utf-8")
    table = text[text.index("const OWNED: &[(&str, &str)] = &["):]
    table = table[:table.index("];")]
    rows = []
    for line in table.splitlines()[1:]:
        line = line.strip().rstrip(",")
        if line.startswith("(") and line.endswith(")"):
            key, bead = (part.strip() for part in line[1:-1].split(",", 1))
            rows.append((key.removeprefix("r").strip('"'), bead.strip('"')))
    return rows


def reference_defect(source):
    return any(space in source for space in (r"\;", r"\:", r"\!"))


def corpus_texts():
    path = Path(os.environ.get("FMN_TEX_CORPUS", ROOT / "corpus" / "tex_corpus.jsonl"))
    if not path.is_file():
        return {}
    texts = {}
    for line in path.open(encoding="utf-8"):
        entry = json.loads(line)
        if entry["mode"] == "math":
            digest = hashlib.sha256(b"math\0" + entry["text"].encode("utf-8")).hexdigest()
            texts[digest] = entry["text"]
    return texts


def run():
    owned = owned_rows()
    texts = corpus_texts()
    checked = excluded = skipped = 0
    failures = []
    for line in FIXTURE.read_text(encoding="utf-8").splitlines():
        if line.startswith("#") or not line.strip():
            continue
        kind, key, count, width, _height, klass, _fonts = line.split("\t")
        source = key if kind == "authored" else texts.get(key)
        if source is None:
            skipped += 1
            continue
        if reference_defect(source) or klass == "traced":
            excluded += 1
            continue
        try:
            error = tex_width(source) / float(width) - 1.0
        except Exception as refusal:  # noqa: BLE001 - a refusal is a failing row
            failures.append(f"{kind} {source!r}: refused: {refusal}")
            continue
        checked += 1
        within = abs(error) <= TOLERANCE[klass]
        bead = next((b for k, b in owned if (key == k if kind == "authored" else key.startswith(k))), None)
        if bead is None and not within:
            failures.append(f"{kind} {source!r} (x{count}): Reference width {width}, portal "
                            f"{error * 100:+.2f}% (tolerance {TOLERANCE[klass] * 100:.0f}%)")
        elif bead is not None and within:
            failures.append(f"{source!r} is within tolerance now ({error * 100:+.2f}%): drop its {bead} exception")
    print(f"tex reference widths (installed wheel): {checked} rows checked, {excluded} Reference "
          f"defects excluded, {skipped} corpus rows skipped without the corpus, {len(failures)} failing")
    assert not (texts and skipped), f"the corpus is present but {skipped} fixture digests are not in it"
    assert checked >= 10, f"only {checked} rows checked"
    assert not failures, "\n".join(failures)


def tex_width(source):
    return m.Tex(source).get_width()


if __name__ in ("__main__", "<run_path>"):
    run()
