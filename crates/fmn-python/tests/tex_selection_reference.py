"""Tex substring selection against the pinned Reference (fm-portal-span-parity-sc4k).

crates/fmn-conformance/fixtures/tex_selection_reference.v1.ndjson holds, for the Reference box
oracle's 111 authored formulas and a deterministic set of selectors, which glyphs the pinned
Reference selects (scripts/capture_tex_selection.py):

- `getitem`: `Tex(s)[sel]`;
- `t2c`: what `Tex(s, t2c={sel: RED})` colours (string selectors);
- `isolate`: `Tex(s, isolate=[sel])[sel]`;
- `count`: `Tex(s).substr_to_path_count(sel)`;
- `matching`: for 50 authored formula pairs, how `TransformMatchingTex(Tex(a), Tex(b))` pairs their
  pieces (each sub-animation's kind and its source and target glyphs), as the Reference runs it and
  with its leftover shape matching disabled (`matching_blocks`).

The portal selects through native source spans; this check runs the same calls. Submobject orders
differ between engines (fm-aia1), so each formula's Reference glyphs are put in correspondence with
the portal's by position: a greedy nearest pairing of their box centres, within CORRESPONDENCE
scene units, which tolerates small layout differences. A selection then compares as a set of glyph
indices. A t2c or isolate formula is read in the plain one on both sides: by index when every
glyph keeps its size at its index, else by box. The Reference's braced isolation can move and
reorder glyphs: an isolated `\\sum` becomes an ordinary atom, so its limits become scripts and
TeX emits the sum first.

Verdicts:
- `match`: the same glyphs, or both engines refuse.
- `reference-defect:C-36`: the Reference's answer contradicts its own label-based answer, and the
  portal gives the label-based one. For a selector it does not isolate, and for every regex, the
  Reference maps each match to submobjects by counting TeX symbols before it
  (`select_unisolated_substring`, `num_tex_symbols`). A match inside a command name (the `i` of
  `\\pi`), a construct whose glyphs dvisvgm emits out of source order (`\\frac`, a superscript
  before a subscript), or a miscounted command (`\\,dx` read as one command) lands on the wrong
  glyphs. The label-based answer is:
  - for a string selector, the Reference's `isolate` selection;
  - for a regex, the union of the Reference's `isolate` selections of the literals it matches
    outside command names;
  - for a count, the Reference's isolated glyph count over the selector's occurrences.
- `reference-refuses:C-37`: the Reference's t2c or isolate raises a LaTeX error, because its
  labelling braces break a `\\left`/`\\right` delimiter, an alignment cell or a physics-package
  macro argument. The portal must then give its own `getitem` answer.
  For a TransformMatchingTex pairing, the Reference's block matching slices the same way: when its
  block pairing morphs a glyph into a different one (its own `has_same_shape_as` is false) while
  every portal transform carries pieces of the same source text (the native span map), the
  Reference's pairing is the defect.
- `behavior-note:BN-05`: a pairing that differs from the Reference's only by its leftover shape
  matching. The Reference pairs any two leftover glyphs of the same outline; the portal fades
  them, because a shape coincidence is not a semantic match. The portal pairing equals the
  Reference's `matching_blocks` record.
- `owned:<bead>`: a known mismatch an open bead owns (OWNED). An owned row that matches fails, so
  the exception goes the day the bead lands.

Anything else fails the check, and at least 95% of rows must be `match`, a C-36 or C-37 Reference
defect, or BN-05. A planted portal whose `getitem` answer is one glyph off must fail it. Run as a
script, every row is printed as NDJSON (`string, selector, mode, ref, fmn, verdict`); the embedded
gauntlet prints the summary and the failing rows only.
"""
import json
import math
import re
import sys
from pathlib import Path

CORRESPONDENCE = 0.3
# A t2c or isolate formula can move glyphs further than the engines differ (an isolated sum's
# scripts move beside it), and sizes tell its glyphs apart.
VARIANT = 0.8
TOLERANCE = 0.06
FLOOR = 0.95
# (formula, selector, mode) -> bead: mismatches an open bead owns.
OWNED = {}
# formula -> bead: an open bead owns every mismatching row of the formula, because its layout is
# off as a whole (the correspondence cannot pair its glyphs). Stale once none mismatch.
OWNED_FORMULAS = {
    # The native CM row follows amsmath; the pinned Reference's wasysym
    # redefinition is narrower. At439a910 this formula remains19.1% wider,
    # so positional correspondence still loses its parts. Keep the open owner.
    r"\iint_D f\,dA": "fm-zxxy",
}
# An environment's name first: alternation takes the first alternative that matches.
_COMMAND = re.compile(r"\\(?:begin|end)\{[^}]*\}|\\[A-Za-z]+")


def _leaves(mobject):
    return [leaf for leaf in mobject.get_family() if len(leaf.get_points()) > 0]


def _box(leaf):
    centre = leaf.get_center()
    return [float(centre[0]), float(centre[1]), float(leaf.get_width()), float(leaf.get_height())]


def _cost(p, q):
    """Centre distance, plus the size difference when both carry sizes: a large glyph never pairs
    with a small one that happens to sit near its centre (a display sum and its limits)."""
    cost = math.dist(p[:2], q[:2])
    if len(p) >= 4 and len(q) >= 4:
        cost += abs(p[2] - q[2]) + abs(p[3] - q[3])
    return cost


def _pairing(a, b, limit):
    """Greedy cheapest one-to-one pairing of box lists `a` -> `b` within `limit` (`_cost`)."""
    pairs = sorted((_cost(p, q), i, j) for i, p in enumerate(a) for j, q in enumerate(b))
    mapping, used = {}, set()
    for distance, i, j in pairs:
        if distance > limit:
            break
        if i in mapping or j in used:
            continue
        mapping[i] = j
        used.add(j)
    return mapping


class _Portal:
    """The portal's formulas, built once: plain leaves and their boxes."""

    def __init__(self, m):
        self.m = m
        self.plain = {}

    def formula(self, source):
        if source not in self.plain:
            tex = self.m.Tex(source)
            self.plain[source] = (tex, [_box(leaf) for leaf in _leaves(tex)])
        return self.plain[source]

    def selection(self, source, key, mode):
        """The portal's selection as indices of its plain formula's leaves."""
        m = self.m
        plain, base = self.formula(source)
        if mode == "getitem":
            tex, selected = plain, _leaves(plain[key])
        elif mode == "isolate":
            tex = m.Tex(source, isolate=[key])
            selected = _leaves(tex[key])
        else:
            tex = m.Tex(source, t2c={key: m.RED})
            red = str(m.RED).lower()
            selected = [leaf for leaf in _leaves(tex) if leaf.get_fill_color().lower() == red]
        chosen = {id(leaf) for leaf in selected}
        leaves = _leaves(tex)
        picked = [i for i, leaf in enumerate(leaves) if id(leaf) in chosen]
        return _variant_indices(picked, [_box(leaf) for leaf in leaves], base)


def _piece_keys(tex):
    """Each leaf's source text, from the native span map."""
    raw = tex.string.encode("utf-8")
    keys = {}
    for ordinal, (start, end) in enumerate(tex._string_sub_spans):
        text = raw[start:end].decode("utf-8", "replace")
        for leaf in _leaves(tex._string_submobject(ordinal)):
            keys[id(leaf)] = text
    return keys


def _portal_pairing(m, first, second):
    source, target = m.Tex(first), m.Tex(second)
    keys = {**_piece_keys(source), **_piece_keys(target)}
    anims = []
    for anim in m.TransformMatchingTex(source, target).anims:
        name = type(anim).__name__
        pieces = lambda mobject: [_box(leaf) for leaf in _leaves(mobject)]  # noqa: E731
        texts = lambda mobject: sorted(keys.get(id(leaf), "?") for leaf in _leaves(mobject))  # noqa: E731
        if name == "FadeOutToPoint":
            anims.append({"kind": "fade_out", "source": pieces(anim.mobject), "target": []})
        elif name == "FadeInFromPoint":
            anims.append({"kind": "fade_in", "source": [], "target": pieces(anim.mobject)})
        else:
            anims.append({"kind": "transform", "source": pieces(anim.mobject),
                          "target": pieces(anim.target_mobject),
                          "same_key": texts(anim.mobject) == texts(anim.target_mobject)
                          and "?" not in texts(anim.mobject)})
    return anims


def _piece_pairs(anims):
    """A pairing at piece level: (source box, target box) for each piece a Transform carries over
    (a group's members align in order, so one Transform of two pieces animates as two Transforms
    of one), plus the faded-out and faded-in pieces."""
    moves, gone, new = [], [], []
    for anim in anims:
        if anim["kind"] == "fade_out":
            gone.extend(anim["source"])
        elif anim["kind"] == "fade_in":
            new.extend(anim["target"])
        elif len(anim["source"]) == len(anim["target"]):
            moves.extend(zip(anim["source"], anim["target"]))
        else:
            # Unequal groups morph as wholes: compare their pieces' boxes.
            moves.append((tuple(map(tuple, anim["source"])), tuple(map(tuple, anim["target"]))))
    return moves, gone, new


def _same_pairing(a, b, limit):
    """Whether two pairings move the same pieces to the same places and fade the same pieces."""
    moves_a, gone_a, new_a = _piece_pairs(a)
    moves_b, gone_b, new_b = _piece_pairs(b)

    def same_boxes(x, y):
        return len(x) == len(y) and len(_pairing(x, y, limit)) == len(x)

    if not (same_boxes(gone_a, gone_b) and same_boxes(new_a, new_b)) or len(moves_a) != len(moves_b):
        return False
    unused = list(range(len(moves_b)))
    for source, target in moves_a:
        for index in unused:
            other_source, other_target = moves_b[index]
            if isinstance(source, tuple) != isinstance(other_source, tuple):
                continue
            if isinstance(source, tuple):
                ok = same_boxes(list(source), list(other_source)) and same_boxes(
                    list(target), list(other_target))
            else:
                ok = _cost(source, other_source) <= limit and _cost(target, other_target) <= limit
            if ok:
                unused.remove(index)
                break
        else:
            return False
    return True


def _in_place(variant, plain):
    """Whether a t2c or isolate formula keeps the plain formula's glyphs at the same indices: the
    same count, each the same size. Bracing can reorder glyphs as well as move them (an isolated
    `\\sum` becomes an ordinary atom, so TeX emits it before its former limits)."""
    return len(variant) == len(plain) and all(
        abs(v[2] - p[2]) < 0.02 and abs(v[3] - p[3]) < 0.02 for v, p in zip(variant, plain)
    )


def _variant_indices(picked, variant, plain):
    """Indices `picked` in a t2c or isolate formula, as indices of the plain formula: by index
    when the glyphs stay in place, else by box (position and size)."""
    if _in_place(variant, plain):
        return sorted(picked)
    mapping = _pairing(variant, plain, VARIANT)
    return sorted(mapping.get(i, -1 - i) for i in picked)


def _reference_indices(row, plain):
    """A Reference selection as indices of its plain formula's glyphs."""
    return _variant_indices(row["selected"], row.get("boxes", plain), plain)


def _matches_outside_names(source, key, kind):
    names = [m.span() for m in _COMMAND.finditer(source)]
    pattern = key if kind == "regex" else re.escape(key)
    for match in re.finditer(pattern, source):
        if not any(a < match.end() and match.start() < b for a, b in names):
            yield match


def _regex_literals(source, pattern):
    commands = [m.span() for m in _COMMAND.finditer(source)]
    literals = []
    for match in re.finditer(pattern, source):
        inside = any(a < match.end() and match.start() < b for a, b in commands)
        if not inside and match.group() not in literals:
            literals.append(match.group())
    return literals


class _PlantedPortal(_Portal):
    """A portal whose `getitem` answer is one glyph off: the check must catch it."""

    def selection(self, source, key, mode):
        picked = super().selection(source, key, mode)
        return [index + 1 for index in picked] if mode == "getitem" else picked


def selection_records(m, fixture_text, portal_class=None):
    lines = [json.loads(line) for line in fixture_text.splitlines() if line.strip()]
    header, rows = lines[0], lines[1:]
    if header.get("schema") != "fmn.tex-selection-reference":
        raise ValueError(f"not a selection fixture: {header}")
    plain = {row["string"]: row["boxes"] for row in rows if "boxes" in row and "mode" not in row}
    reference = {}
    for row in rows:
        if "selector" in row:
            if isinstance(row.get("selected"), list):
                row["indices"] = _reference_indices(row, plain[row["string"]])
            reference[(row["string"], row["selector"]["value"], row["mode"])] = row
    portal = (portal_class or _Portal)(m)
    correspondence = {}

    def to_portal(source, indices):
        """Reference plain indices -> portal plain indices (unpaired glyphs stay negative)."""
        if source not in correspondence:
            _, base = portal.formula(source)
            correspondence[source] = _pairing(plain[source], base, CORRESPONDENCE)
        mapping = correspondence[source]
        return sorted(mapping.get(i, -1000 - i) for i in indices)

    def owned(record, verdict):
        owner = OWNED.get((record["string"], record["selector"], record["mode"]))
        if owner is None:
            whole = OWNED_FORMULAS.get(record["string"])
            return "owned:" + whole if whole is not None and verdict == "mismatch" else verdict
        return "stale-owned:" + owner if verdict == "match" else "owned:" + owner

    blocks = {tuple(row["pair"]): row for row in rows if row.get("mode") == "matching_blocks"}
    records = []
    for row in rows:
        if row.get("mode") == "matching":
            first, second = row["pair"]
            record = {"string": f"{first} -> {second}", "selector": "", "mode": "matching"}
            try:
                ours, fmn_error = _portal_pairing(m, first, second), None
            except Exception as error:  # noqa: BLE001
                ours, fmn_error = None, f"{type(error).__name__}: {str(error)[:120]}"
            ref_error = row.get("error")
            record["ref_indices"] = ref_error or len(row["anims"])
            record["fmn_indices"] = fmn_error or len(ours)
            if ref_error or fmn_error:
                verdict = "match" if (ref_error and fmn_error) else "mismatch"
            elif _same_pairing(row["anims"], ours, CORRESPONDENCE):
                verdict = "match"
            elif _same_pairing(blocks.get((first, second), {}).get("anims", []), ours, CORRESPONDENCE):
                verdict = "behavior-note:BN-05"
            elif (any(a["kind"] == "transform" and not a.get("same_shape", True)
                      for a in blocks.get((first, second), {}).get("anims", []))
                  and all(a["same_key"] for a in ours if a["kind"] == "transform")):
                # The Reference's block match morphs a glyph into a different glyph (its own
                # has_same_shape_as says so): its symbol-count slices missed the matched symbols.
                # Every portal transform carries pieces of the same source text.
                verdict = "reference-defect:C-36"
            else:
                verdict = "mismatch"
            record["verdict"] = owned(record, verdict)
            records.append(record)
            continue
        if "selector" not in row or row["mode"] == "matching_blocks":
            continue
        source, selector, mode = row["string"], row["selector"], row["mode"]
        value = selector["value"]
        key = re.compile(value) if selector["kind"] == "regex" else value
        record = {"string": source, "selector": value, "mode": mode}
        try:
            if mode == "count":
                ours = int(portal.formula(source)[0].substr_to_path_count(value))
            else:
                ours = portal.selection(source, key, mode)
            fmn_error = None
        except Exception as error:  # noqa: BLE001 - a refusal is a result to compare
            ours, fmn_error = None, f"{type(error).__name__}: {str(error)[:120]}"
        if mode == "count":
            theirs = row.get("count")
            ref_error = theirs if isinstance(theirs, str) else None
        else:
            ref_error = row.get("error")
            theirs = None if ref_error else to_portal(source, row["indices"])
        record["ref_indices"] = ref_error or theirs
        record["fmn_indices"] = fmn_error or ours
        if ref_error and fmn_error:
            verdict = "match"
        elif fmn_error:
            verdict = "mismatch"
        elif ref_error:
            verdict = "mismatch"
            if mode in ("t2c", "isolate") and ref_error.startswith("LatexError"):
                if ours == portal.selection(source, key, "getitem"):
                    verdict = "reference-refuses:C-37"
        else:
            verdict = "match" if ours == theirs else "mismatch"
        if verdict == "mismatch" and not fmn_error and not ref_error:
            if mode == "count":
                isolate = reference.get((source, value, "isolate"), {})
                occurrences = len(re.findall(re.escape(value), source))
                if "indices" in isolate and occurrences and len(isolate["indices"]) % occurrences == 0:
                    truth = len(isolate["indices"]) // occurrences
                    if theirs != truth and ours == truth:
                        verdict = "reference-defect:C-36"
            elif selector["kind"] == "regex":
                truth, complete = [], True
                for literal in _regex_literals(source, value):
                    isolate = reference.get((source, literal, "isolate"), {})
                    if "indices" not in isolate:
                        complete = False
                        break
                    truth.extend(isolate["indices"])
                if complete:
                    truth = to_portal(source, sorted(set(truth)))
                    if truth != theirs and ours == truth:
                        verdict = "reference-defect:C-36"
            elif mode == "getitem":
                isolate = reference.get((source, value, "isolate"), {})
                if "indices" in isolate:
                    truth = to_portal(source, isolate["indices"])
                    if truth != theirs and ours == truth:
                        verdict = "reference-defect:C-36"
            if verdict == "mismatch" and mode == "getitem":
                # No label-based answer (the Reference's isolate refused, C-37): a selector of
                # single characters selects one glyph per match outside command and environment
                # names. The Reference selected glyphs for matches inside them as well.
                literals = re.findall(key if selector["kind"] == "regex" else re.escape(value), source)
                if literals and all(len(literal) == 1 for literal in literals):
                    outside = sum(1 for _ in _matches_outside_names(source, key, selector["kind"]))
                    if len(theirs) > outside and len(ours) == outside:
                        verdict = "reference-defect:C-36"
        record["verdict"] = owned(record, verdict)
        records.append(record)
    for formula, bead in OWNED_FORMULAS.items():
        if not any(r["string"] == formula and r["verdict"].startswith("owned:") for r in records):
            records.append({"string": formula, "selector": "", "mode": "owned-formula",
                            "ref_indices": None, "fmn_indices": None, "verdict": "stale-owned:" + bead})
    return records


def run_tex_selection_reference(fixture_text=None, print_rows=False):
    """The check, in the embedded portal or an installed wheel. Returns the summary, or raises
    with every failing row."""
    import manimlib as m

    if fixture_text is None:
        # Run as a script against the checkout; the embedded gauntlet passes the fixture in.
        root = Path(__file__).resolve().parents[3]
        fixture = root / "crates" / "fmn-conformance" / "fixtures" / "tex_selection_reference.v1.ndjson"
        fixture_text = fixture.read_text(encoding="utf-8")
    records = selection_records(m, fixture_text)
    for record in records if print_rows else ():
        print(json.dumps({"schema": "fmn.tex-selection.v1", **record}, ensure_ascii=False))
    counts = {}
    for record in records:
        kind = record["verdict"].split(":")[0]
        counts[kind] = counts.get(kind, 0) + 1
    explained = (counts.get("match", 0) + counts.get("reference-defect", 0)
                 + counts.get("reference-refuses", 0) + counts.get("behavior-note", 0))
    summary = {
        "schema": "fmn.tex-selection.v1",
        "rows": len(records),
        "match": counts.get("match", 0),
        "reference_defect": counts.get("reference-defect", 0) + counts.get("reference-refuses", 0),
        "behavior_note": counts.get("behavior-note", 0),
        "owned": counts.get("owned", 0),
        "share": round(explained / max(len(records), 1), 4),
    }
    print(json.dumps(summary))
    failing = [r for r in records if r["verdict"] == "mismatch" or r["verdict"].startswith("stale")]
    problems = []
    planted = [r for r in selection_records(m, fixture_text, _PlantedPortal) if r["verdict"] == "mismatch"]
    summary["planted_mismatches"] = len(planted)
    print(json.dumps({"schema": "fmn.tex-selection.v1", "planted": "getitem one glyph off",
                      "mismatches": len(planted)}))
    if len(planted) < 100:
        problems.append(f"a planted one-glyph shift of getitem raised only {len(planted)} mismatches")
    if failing:
        problems.append(f"{len(failing)} rows mismatch the Reference with no owner or recorded cause")
    if summary["share"] < FLOOR:
        problems.append(f"{summary['share']:.2%} of rows agree, below {FLOOR:.0%}")
    if problems:
        detail = "\n".join(json.dumps(r, ensure_ascii=False) for r in failing[:40])
        raise AssertionError("; ".join(problems) + "\n" + detail)
    return summary


def run():
    run_tex_selection_reference(print_rows=True)


if __name__ in ("__main__", "<run_path>"):
    try:
        run()
    except AssertionError as error:
        print(error, file=sys.stderr)
        raise SystemExit(1) from None
