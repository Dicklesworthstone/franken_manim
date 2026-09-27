"""Structural facts: one extractor and one diff for the Reference and the portal (fm-5wq.36).

This module runs **unmodified** under the pinned Reference (3b1b/manim @
6199a00d, `scripts/manim_ref`) and under FrankenManim's `manimlib` portal. It
touches only the public manimlib API: `submobjects`, `get_points`,
`get_bounding_box`, `data`, the style getters, and a small declared table of
zero-argument getters. So the same code measures both engines, and the class
sweep, the corpus differential and the differential fuzzer (fm-5wq.26, .33,
.34) share one answer about quantization, ordering and naming.

Schema `fmn.structural-facts` version 1 (docs/ratchet/STRUCTURAL_FACTS.md):
one JSON object per subject (a construction, a class, or a scene's
`self.mobjects` at `tear_down`). The object holds depth-first members keyed
by index path, and the canonical encoding holds only integers and strings.
Every length is an integer count of quanta: `QUANTA_PER_UNIT` per scene unit,
rounded half to even.

Command line:
  python structural_facts.py extract --engine-id ID [--points full|digest] OUT.ndjson
      Construct every entry of CONSTRUCTIONS in the running engine and write
      one fact line per subject.
  python structural_facts.py diff REFERENCE.ndjson PORTAL.ndjson [--exclusions X.json]
      Compare two fact files and write one verdict line per subject.
      Exit 0 = no unexcluded difference, no stale open-bead exclusion.
"""
from __future__ import annotations

import argparse
import fnmatch
import hashlib
import json
import math
import sys
import traceback
from pathlib import Path

SCHEMA = "fmn.structural-facts"
VERSION = 1
DIFF_SCHEMA = "fmn.structural-diff"
QUANTA_PER_UNIT = 1000
# The two engines compute in f64 along different paths. One quantum of slack
# absorbs values that straddle a rounding boundary. A move of two or more
# quanta (2e-3 scene units) is a difference.
TOLERANCE_QUANTA = 1
MAX_MEMBERS = 20_000
MAX_FULL_POINTS = 200_000
# Sentinels keep non-finite coordinates integral and distinguishable.
_NAN, _POS_INF, _NEG_INF = -(2**62), 2**62 - 1, -(2**62 - 1)
_LIMIT = 2**61

# Zero-argument getters recorded when a member's public class chain contains
# the key. Values are quantized when numeric. This table is part of the
# schema: changing it is a version bump.
GETTERS = {
    "Arc": ("get_arc_center", "get_start_angle"),
    "Circle": ("get_radius",),
    "Line": ("get_start", "get_end", "get_length", "get_angle"),
    "Polygon": ("get_vertices",),
    "DecimalNumber": ("get_value",),
    "NumberLine": ("x_min", "x_max", "x_step"),
    "CoordinateSystem": ("get_origin",),
}

# The fixed construction set: source text evaluated in the manimlib namespace
# of whichever engine is running. Identifiers are stable; append-only.
CONSTRUCTIONS = (
    ("square", "Square()"),
    ("circle", "Circle(radius=1.5)"),
    ("rectangle", "Rectangle(width=3, height=1)"),
    ("line", "Line(LEFT, RIGHT + UP)"),
    ("arrow", "Arrow(LEFT, RIGHT)"),
    ("dot", "Dot(UP)"),
    ("arc", "Arc(start_angle=0, angle=PI / 3, radius=2)"),
    ("polygon", "Polygon(ORIGIN, RIGHT, UP)"),
    ("regular_polygon", "RegularPolygon(6)"),
    ("star", "Star()"),
    ("annulus", "Annulus()"),
    ("vgroup_arranged", "VGroup(Square(), Circle(), Triangle()).arrange(RIGHT)"),
    ("shifted_scaled", "Square().shift(2 * RIGHT).scale(0.5).rotate(PI / 7)"),
    ("styled", "Circle().set_fill(RED, opacity=0.5).set_stroke(BLUE, width=6)"),
    ("text", 'Text("Hello")'),
    ("tex", r'Tex(r"x^2 + \frac{1}{2}")'),
    ("decimal_number", "DecimalNumber(3.14159, num_decimal_places=3)"),
    ("number_line", "NumberLine((-3, 3, 1))"),
    ("axes", "Axes((-2, 2), (-1, 1))"),
    ("number_plane", "NumberPlane((-2, 2), (-1, 1))"),
    ("surrounding_rectangle", "SurroundingRectangle(Circle())"),
    ("brace", "Brace(Square(), DOWN)"),
    ("sphere", "Sphere()"),
    ("cone", "Cone()"),
    ("vcube", "VCube()"),
)


# ---------------------------------------------------------------- encoding


def canonical(value) -> str:
    """The one serialization: sorted keys, no whitespace, integers and strings only."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def _quantize_scalar(x) -> int:
    x = float(x)
    if math.isnan(x):
        return _NAN
    if math.isinf(x):
        return _POS_INF if x > 0 else _NEG_INF
    q = x * QUANTA_PER_UNIT
    if abs(q) >= _LIMIT:
        return _LIMIT if q > 0 else -_LIMIT
    # round() on a float is round-half-to-even.
    return int(round(q))


def quantize(value):
    """Quantize a number or a (nested) sequence of numbers to integer quanta."""
    if hasattr(value, "tolist"):
        value = value.tolist()
    if isinstance(value, bool) or value is None or isinstance(value, str):
        return value
    if isinstance(value, (int, float)):
        return _quantize_scalar(value)
    if isinstance(value, (list, tuple)):
        return [quantize(item) for item in value]
    return repr(value)


def _flatten(values):
    for item in values:
        if isinstance(item, list):
            yield from _flatten(item)
        else:
            yield item


def digest_ints(values) -> str:
    """sha256 over the flattened integers, each as a signed 64-bit little-endian word."""
    h = hashlib.sha256()
    for item in _flatten(values):
        h.update(int(item).to_bytes(8, "little", signed=True))
    return h.hexdigest()


# ---------------------------------------------------------------- extraction


def _engine_namespace():
    import manimlib

    return manimlib


def public_mro(cls, namespace) -> list[str]:
    """Names in the class chain that the engine's manimlib exports under the same name.

    Private names, and classes from the standard library (typing.Generic, abc.ABC)
    that leak into the namespace, are not part of the manim API surface.
    """
    stdlib = getattr(sys, "stdlib_module_names", frozenset())
    names = []
    for klass in cls.__mro__:
        name = klass.__name__
        if name.startswith("_") or getattr(namespace, name, None) is not klass:
            continue
        if klass.__module__.split(".")[0] in stdlib or klass.__module__ == "builtins":
            continue
        names.append(name)
    return names


def _call_getter(mob, name):
    attr = getattr(mob, name, _ABSENT)
    if attr is _ABSENT:
        return "absent"
    try:
        value = attr() if callable(attr) else attr
    except Exception as error:  # noqa: BLE001 - a raising getter is itself a fact
        return f"raises {type(error).__name__}"
    return quantize(value)


_ABSENT = object()


def _style(mob, namespace):
    vmobject = getattr(namespace, "VMobject", None)
    if vmobject is None or not isinstance(mob, vmobject):
        return None
    style = {}
    for key, getter in (
        ("fill_color", "get_fill_color"),
        ("fill_opacity", "get_fill_opacity"),
        ("stroke_color", "get_stroke_color"),
        ("stroke_width", "get_stroke_width"),
        ("stroke_opacity", "get_stroke_opacity"),
    ):
        value = _call_getter(mob, getter)
        if isinstance(value, str) and value.startswith("#"):
            value = value.upper()
        style[key] = value
    return style


def _member_facts(mob, path, namespace, points_mode):
    facts = {"path": path, "class": type(mob).__name__, "mro": public_mro(type(mob), namespace)}
    try:
        points = mob.get_points()
        rows = points.tolist() if hasattr(points, "tolist") else [list(p) for p in points]
    except Exception as error:  # noqa: BLE001
        facts["points"] = f"raises {type(error).__name__}"
        rows = None
    if rows is not None:
        q = quantize(rows)
        facts["n_points"] = len(q)
        facts["points_sha256"] = digest_ints(q)
        if points_mode == "full" and len(q) <= MAX_FULL_POINTS:
            facts["points"] = q
    try:
        facts["bbox"] = quantize(mob.get_bounding_box())
    except Exception as error:  # noqa: BLE001
        facts["bbox"] = f"raises {type(error).__name__}"
    data = getattr(mob, "data", None)
    names = getattr(getattr(data, "dtype", None), "names", None)
    facts["data_fields"] = list(names) if names else None
    facts["z_index"] = quantize(getattr(mob, "z_index", None))
    string = getattr(mob, "string", None)
    if isinstance(string, str):
        facts["string"] = string
    style = _style(mob, namespace)
    if style is not None:
        facts["style"] = style
    getters = {}
    for klass in facts["mro"]:
        for name in GETTERS.get(klass, ()):
            getters[name] = _call_getter(mob, name)
    if getters:
        facts["getters"] = getters
    return facts


def extract(subject: str, roots, *, namespace=None, points_mode: str = "digest") -> dict:
    """Facts for a list of root mobjects, members in depth-first order.

    Paths are dot-joined submobject indices under the root's index, so the
    first root is "0", its second child "0.1".
    """
    if points_mode not in ("digest", "full"):
        raise ValueError(f"points_mode must be 'digest' or 'full', not {points_mode!r}")
    namespace = namespace or _engine_namespace()
    members = []
    truncated = False
    stack = [(str(index), root) for index, root in reversed(list(enumerate(roots)))]
    while stack:
        path, mob = stack.pop()
        if len(members) >= MAX_MEMBERS:
            truncated = True
            break
        members.append(_member_facts(mob, path, namespace, points_mode))
        children = list(getattr(mob, "submobjects", ()) or ())
        stack.extend((f"{path}.{i}", child) for i, child in reversed(list(enumerate(children))))
    record = {
        "schema": SCHEMA,
        "version": VERSION,
        "quanta_per_unit": QUANTA_PER_UNIT,
        "subject": subject,
        "members": members,
    }
    if truncated:
        record["truncated_at"] = MAX_MEMBERS
    return record


def extract_error(subject: str, error: BaseException) -> dict:
    """A construction that raised is a fact too: its exception class and first line."""
    message = str(error).splitlines()[0][:200] if str(error) else ""
    return {
        "schema": SCHEMA,
        "version": VERSION,
        "quanta_per_unit": QUANTA_PER_UNIT,
        "subject": subject,
        "error": f"{type(error).__name__}: {message}",
    }


def engine_identity(engine_id: str) -> dict:
    namespace = _engine_namespace()
    portal = bool(getattr(namespace, "__franken_manim__", False))
    return {"engine": "franken_manim" if portal else "reference", "engine_id": engine_id}


def extract_constructions(engine_id: str, points_mode: str = "digest", only=None):
    """Yield one fact record per construction in the fixed set."""
    namespace = _engine_namespace()
    scope = {name: getattr(namespace, name) for name in dir(namespace) if not name.startswith("_")}
    identity = engine_identity(engine_id)
    for subject, source in CONSTRUCTIONS:
        if only is not None and subject not in only:
            continue
        try:
            mob = eval(source, dict(scope))  # noqa: S307 - fixed, checked-in source text
            record = extract(subject, [mob], namespace=namespace, points_mode=points_mode)
        except Exception as error:  # noqa: BLE001
            record = extract_error(subject, error)
        record["source"] = source
        record["engine"] = identity
        yield record


def scene_hook(scene_class, sink):
    """Return a subclass of `scene_class` whose `tear_down` records `self.mobjects`.

    Both engines call `tear_down` after `construct` (Reference scene.py:164). The
    subclass keeps the user's name so tracebacks and outputs stay recognizable.
    `sink` receives the fact record.
    """

    def tear_down(self):
        try:
            sink(extract(type(self).__name__, list(self.mobjects)))
        finally:
            super(hooked, self).tear_down()

    hooked = type(scene_class.__name__, (scene_class,), {"tear_down": tear_down})
    hooked.__module__ = scene_class.__module__
    hooked.__qualname__ = scene_class.__qualname__
    return hooked


# ---------------------------------------------------------------- diff


def load_exclusions(path) -> list[dict]:
    rows = json.loads(Path(path).read_text())["exclusions"]
    for row in rows:
        missing = {"id", "kind", "ref", "fact", "reason"} - set(row)
        if missing:
            raise ValueError(f"exclusion {row.get('id')!r} lacks {sorted(missing)}")
        if row["kind"] not in ("behavior-note", "open-bead"):
            raise ValueError(f"exclusion {row['id']!r}: kind must be behavior-note or open-bead")
    return rows


def _matches(value, pattern) -> bool:
    """A row field is one glob or a list of globs; any match admits the value."""
    patterns = [pattern] if isinstance(pattern, str) else pattern
    return any(fnmatch.fnmatchcase(value, p) for p in patterns)


def _excluded_by(rows, subject, member_class, fact):
    """The first row covering this difference. Rows name the Reference-side class."""
    for row in rows:
        if (_matches(subject, row.get("subject", "*"))
                and _matches(member_class or "", row.get("class", "*"))
                and _matches(fact, row["fact"])):
            return row
    return None


def _within_tolerance(a, b) -> bool:
    """Equality with TOLERANCE_QUANTA slack on integers (quantized lengths only)."""
    if isinstance(a, list) and isinstance(b, list):
        return len(a) == len(b) and all(_within_tolerance(x, y) for x, y in zip(a, b))
    if isinstance(a, int) and isinstance(b, int) and not isinstance(a, bool) and not isinstance(b, bool):
        return abs(a - b) <= TOLERANCE_QUANTA
    return a == b


# Facts whose integers are quantized lengths or angles get the rounding slack;
# every other fact (class names, counts, field names, strings) is exact.
QUANTIZED_FACTS = frozenset({"points", "bbox", "z_index", "getters", "style"})


def _same(key, a, b) -> bool:
    return _within_tolerance(a, b) if key in QUANTIZED_FACTS else a == b


def _member_differences(ref, portal):
    """Yield (fact, reference_value, portal_value) for one aligned member pair."""
    for key in sorted(set(ref) | set(portal)):
        if key in ("path", "points_sha256"):
            continue
        a, b = ref.get(key, "absent"), portal.get(key, "absent")
        if key == "points" and (a == "absent" or b == "absent"):
            continue  # digest mode on one side: compare the digest below
        if isinstance(a, dict) and isinstance(b, dict):
            for sub in sorted(set(a) | set(b)):
                x, y = a.get(sub, "absent"), b.get(sub, "absent")
                if not _same(key, x, y):
                    yield f"{key}.{sub}", x, y
        elif not _same(key, a, b):
            yield key, a, b
    if "points" not in ref or "points" not in portal:
        if ref.get("points_sha256") != portal.get("points_sha256"):
            yield "points_sha256", ref.get("points_sha256"), portal.get("points_sha256")


def diff_subject(ref: dict, portal: dict, exclusions=(), limit: int = 50) -> dict:
    """Compare one subject's facts. Differences are reported in depth-first path order."""
    subject = ref.get("subject", portal.get("subject"))
    differences, excluded = [], {}

    def note(path, member_class, fact, a, b):
        row = _excluded_by(exclusions, subject, member_class, fact)
        if row is not None:
            excluded[row["id"]] = excluded.get(row["id"], 0) + 1
        elif len(differences) < limit:
            differences.append({"path": path, "class": member_class, "fact": fact,
                                "reference": _short(a), "portal": _short(b)})
        else:
            differences.append(None)

    if "error" in ref or "error" in portal:
        a, b = ref.get("error", "none"), portal.get("error", "none")
        if a.split(":")[0] != b.split(":")[0]:
            note("", None, "error", a, b)
    else:
        by_path = {m["path"]: m for m in portal["members"]}
        seen = set()
        for member in ref["members"]:
            other = by_path.get(member["path"])
            if other is None:
                note(member["path"], member["class"], "member", member["class"], "absent")
                continue
            seen.add(member["path"])
            for fact, a, b in _member_differences(member, other):
                note(member["path"], member["class"], fact, a, b)
        for member in portal["members"]:
            if member["path"] not in seen:
                note(member["path"], member["class"], "member", "absent", member["class"])
    shown = [d for d in differences if d is not None]
    verdict = "differs" if differences else ("equal-with-exclusions" if excluded else "equal")
    return {
        "schema": DIFF_SCHEMA,
        "version": VERSION,
        "subject": subject,
        "verdict": verdict,
        "difference_count": len(differences),
        "first_difference": shown[0] if shown else None,
        "differences": shown,
        "excluded": dict(sorted(excluded.items())),
    }


def _short(value):
    text = canonical(value)
    return value if len(text) <= 240 else f"{text[:200]}… ({len(text)} chars, sha256 {hashlib.sha256(text.encode()).hexdigest()[:16]})"


def diff_files(reference_lines, portal_lines, exclusions=()):
    """Verdict records for every subject, then a summary with stale open-bead exclusions."""
    ref = {r["subject"]: r for r in reference_lines}
    portal = {r["subject"]: r for r in portal_lines}
    results, used = [], set()
    for subject in list(ref) + [s for s in portal if s not in ref]:
        if subject not in ref or subject not in portal:
            results.append({"schema": DIFF_SCHEMA, "version": VERSION, "subject": subject,
                            "verdict": "one-sided",
                            "present_in": "reference" if subject in ref else "portal"})
            continue
        result = diff_subject(ref[subject], portal[subject], exclusions)
        used.update(result["excluded"])
        results.append(result)
    stale = sorted(row["id"] for row in exclusions if row["kind"] == "open-bead" and row["id"] not in used)
    counts = {}
    for result in results:
        counts[result["verdict"]] = counts.get(result["verdict"], 0) + 1
    summary = {"schema": DIFF_SCHEMA, "version": VERSION, "summary": dict(sorted(counts.items())),
               "stale_open_bead_exclusions": stale}
    return results, summary


def parse_ndjson(text: str):
    """Fact lines; `#` lines carry provenance and are skipped."""
    return [json.loads(line) for line in text.splitlines() if line.strip() and not line.startswith("#")]


def read_ndjson(path):
    return parse_ndjson(Path(path).read_text(encoding="utf-8"))


def provenance_line(engine_id: str, argv) -> str:
    """The `#` header of an extract: enough to regenerate the file on purpose."""
    try:
        import numpy

        numpy_version = numpy.__version__
    except ImportError:  # pragma: no cover - both engines ship NumPy
        numpy_version = None
    return "# " + canonical({
        "schema": SCHEMA, "version": VERSION, "engine": engine_identity(engine_id),
        "python": sys.version.split()[0], "numpy": numpy_version,
        "command": ["structural_facts.py", *argv],
    })


def check_constructions(reference_text: str, exclusions_text: str, engine_id: str) -> dict:
    """Extract the fixed construction set in the running engine and diff it.

    This is the gate and e2e entry point. It returns the diff summary plus
    `compared` (subjects on both sides) and `failing` (subjects that differ
    or are one-sided). A zero-subject comparison is a failure, never a pass.
    """
    exclusions = json.loads(exclusions_text)["exclusions"]
    reference = parse_ndjson(reference_text)
    portal = list(extract_constructions(engine_id, points_mode="full"))
    results, summary = diff_files(reference, portal, exclusions)
    compared = sum(1 for r in results if r["verdict"] != "one-sided")
    failing = [r for r in results if r["verdict"] in ("differs", "one-sided")]
    summary.update(compared=compared, failing=failing)
    if compared == 0:
        summary["failing"].append({"subject": "*", "verdict": "no-subjects-compared"})
    return summary


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    ext = sub.add_parser("extract")
    ext.add_argument("--engine-id", required=True)
    ext.add_argument("--points", choices=("digest", "full"), default="digest")
    ext.add_argument("--only", nargs="*")
    ext.add_argument("out")
    dif = sub.add_parser("diff")
    dif.add_argument("reference")
    dif.add_argument("portal")
    dif.add_argument("--exclusions")
    chk = sub.add_parser("check", help="extract here and diff against a Reference fact file")
    chk.add_argument("--engine-id", required=True)
    chk.add_argument("--reference", required=True)
    chk.add_argument("--exclusions", required=True)
    original = list(sys.argv[1:] if argv is None else argv)
    args = parser.parse_args(argv)
    # The Reference's manimlib parses sys.argv when imported (manimlib/config.py).
    # It must see a bare invocation, not this tool's flags.
    sys.argv = sys.argv[:1]
    if args.command == "extract":
        out = Path(args.out)
        if out.exists():
            print(f"refusing to overwrite {out}", file=sys.stderr)
            return 2
        lines = [canonical(r) for r in extract_constructions(args.engine_id, args.points, args.only)]
        header = provenance_line(args.engine_id, original)
        out.write_text("".join(line + "\n" for line in [header, *lines]), encoding="utf-8")
        print(canonical({"schema": SCHEMA, "written": str(out), "subjects": len(lines)}))
        return 0
    if args.command == "check":
        summary = check_constructions(Path(args.reference).read_text(encoding="utf-8"),
                                      Path(args.exclusions).read_text(encoding="utf-8"),
                                      args.engine_id)
        print(canonical(summary))
        return 1 if summary["failing"] or summary["stale_open_bead_exclusions"] else 0
    exclusions = load_exclusions(args.exclusions) if args.exclusions else []
    results, summary = diff_files(read_ndjson(args.reference), read_ndjson(args.portal), exclusions)
    for result in results:
        print(canonical(result))
    print(canonical(summary))
    failing = any(r["verdict"] in ("differs", "one-sided") for r in results)
    return 1 if failing or summary["stale_open_bead_exclusions"] else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except BrokenPipeError:
        sys.exit(1)
    except Exception:  # noqa: BLE001
        traceback.print_exc()
        sys.exit(3)
