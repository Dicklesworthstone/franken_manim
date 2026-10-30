#!/usr/bin/env python3
"""Differential API fuzzer: random manim programs in the Reference and the portal (fm-5wq.34).

A seeded generator writes programs over a typed grammar of the manim API:
constructors with parameters drawn from their documented domains, positional
operations, style setters, copy/become/add/remove, live point views, and
`self.play` of `.animate` chains and animations. Each batch of programs runs
once in each engine through its own CLI (`api_fuzz_scene.py`: the Reference in
skip mode, the portal construct-only,
which measures every program with the shared structural-facts extractor of
fm-5wq.36). The facts are compared with the same diff and the same exclusion
table as the corpus differential; rows apply here when their scope is `any` or
`fuzz`, and every row cites a Behavior Note, an ADR or an open bead.

A differing program is delta-debugged (ddmin over its statements, each round's
candidates run as one batch per engine) to a minimal program that still shows
the same first difference, and written out with a ready-to-paste regression
test.

Generated programs and facts stay under the gitignored frames/ directory.

Usage (RUN is conventionally frames/api_fuzz/<name>):
  api_differential_fuzz.py campaign --seed 1 --count 1000 --out RUN [--jobs 8]
  api_differential_fuzz.py generate --seed 1 --count 200 --out RUN
  api_differential_fuzz.py run RUN [--jobs 8]
  api_differential_fuzz.py diff RUN
  api_differential_fuzz.py minimize RUN [--limit 20]

Engines: --portal-python (a venv with the portal wheel) and --reference-python
(the pinned Reference's venv, run under xvfb-run with PYTHONPATH at
--reference-root). Each engine gets one thread.
"""
from __future__ import annotations

import argparse
import concurrent.futures
import dataclasses
import fnmatch
import hashlib
import json
import os
import pathlib
import random
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
TOOLS = ROOT / "crates" / "fmn-conformance" / "python"
SCENE = TOOLS / "api_fuzz_scene.py"
EXCLUSIONS = ROOT / "crates" / "fmn-conformance" / "fixtures" / "structural_facts" / "exclusions.json"
sys.path.insert(0, str(TOOLS))
import structural_facts as sf  # noqa: E402

LOG_SCHEMA = "fmn.api-fuzz"
LOG_VERSION = 1
GENERATOR_VERSION = 3
ONE_THREAD = {"OMP_NUM_THREADS": "1", "OPENBLAS_NUM_THREADS": "1", "MKL_NUM_THREADS": "1"}
SCOPE = "fuzz"

# ---------------------------------------------------------------------------
# The grammar
# ---------------------------------------------------------------------------

DIRECTIONS = ["UP", "DOWN", "LEFT", "RIGHT", "UL", "UR", "DL", "DR"]
AXES = ["OUT", "UP", "RIGHT", "np.array([1.0, 1.0, 0.0])"]
COLORS = ["BLUE", "RED", "GREEN", "YELLOW", "TEAL", "WHITE", '"#336699"']
FLAT, SOLID = "flat", "solid"


def _num(rng, lo, hi, places=2):
    return round(rng.uniform(lo, hi), places)


def _point(rng, span=3.0):
    return f"np.array([{_num(rng, -span, span)}, {_num(rng, -span, span)}, 0.0])"


def _vector(rng):
    if rng.random() < 0.5:
        return f"{_num(rng, 0.1, 2.5)} * {rng.choice(DIRECTIONS)}"
    return _point(rng, 2.0)


def _axis(rng):
    return f", axis={rng.choice(AXES)}" if rng.random() < 0.5 else ""


@dataclasses.dataclass(frozen=True)
class Constructor:
    """One constructor: its class, the variable kind it makes, and its arguments."""
    cls: str
    kind: str
    args: object  # rng -> argument source text


CONSTRUCTORS = [
    Constructor("Circle", FLAT, lambda r: f"radius={_num(r, 0.2, 2.5)}"),
    Constructor("Square", FLAT, lambda r: f"side_length={_num(r, 0.2, 3.0)}"),
    Constructor("Rectangle", FLAT, lambda r: f"width={_num(r, 0.2, 4.0)}, height={_num(r, 0.2, 3.0)}"),
    Constructor("Triangle", FLAT, lambda r: ""),
    Constructor("RegularPolygon", FLAT, lambda r: f"n={r.randint(3, 9)}"),
    Constructor("Line", FLAT, lambda r: f"{_point(r)}, {_point(r)}"),
    Constructor("Arrow", FLAT, lambda r: f"{_point(r)}, {_point(r)}, buff={_num(r, 0.0, 0.4)}"),
    Constructor("DashedLine", FLAT, lambda r: f"{_point(r)}, {_point(r)}"),
    Constructor("Dot", FLAT, lambda r: f"point={_point(r)}"),
    # BN-09: an explicit n_components is honored verbatim in both engines, while the default
    # density differs by design (ceil(16|θ|/TAU) against the Reference's three rules).
    Constructor("Arc", FLAT, lambda r: f"start_angle={_num(r, -3.0, 3.0)}, angle={_num(r, 0.3, 6.0)}, "
                                       f"radius={_num(r, 0.2, 2.5)}, n_components={r.randint(2, 12)}"),
    Constructor("Ellipse", FLAT, lambda r: f"width={_num(r, 0.3, 4.0)}, height={_num(r, 0.3, 3.0)}"),
    Constructor("Polygon", FLAT, lambda r: ", ".join(_point(r) for _ in range(r.randint(3, 5)))),
    Constructor("Sphere", SOLID, lambda r: f"radius={_num(r, 0.3, 2.0)}, resolution=(8, 8)"),
    Constructor("Cylinder", SOLID, lambda r: f"height={_num(r, 0.5, 3.0)}, radius={_num(r, 0.2, 1.5)}, "
                                             f"resolution=(8, 4){_axis(r)}"),
    Constructor("Cone", SOLID, lambda r: f"height={_num(r, 0.5, 3.0)}, radius={_num(r, 0.2, 1.5)}, "
                                         f"resolution=(8, 4){_axis(r)}"),
    Constructor("Torus", SOLID, lambda r: f"r1={_num(r, 1.0, 2.5)}, r2={_num(r, 0.1, 0.9)}, resolution=(8, 8)"),
    Constructor("Cube", SOLID, lambda r: f"side_length={_num(r, 0.3, 2.5)}"),
    Constructor("Prism", SOLID, lambda r: f"width={_num(r, 0.3, 2.5)}, height={_num(r, 0.3, 2.5)}, "
                                          f"depth={_num(r, 0.3, 2.5)}"),
    Constructor("VCube", FLAT, lambda r: f"side_length={_num(r, 0.3, 2.5)}"),
]
BY_CLASS = {c.cls: c for c in CONSTRUCTORS}

# Left out on purpose, each with its reason, so a campaign's differences are the portal's:
# - Sector, AnnularSector, Annulus: their arcs take no n_components, so BN-09's density
#   differs by design, and moves every box laid out against them;
# - RoundedRectangle: corner arcs drawn through add_arc_to (BN-09; open bead fm-c1up);
# - VMobject.insert_n_curves: the same greedy rule in both engines, but a circle's equal
#   chords tie, and the first-index tie breaks on ulp-level noise that differs between them;
# - a mobject in two groups (a member reachable twice): the Reference's get_family does
#   not deduplicate, so a group operation moves it once per path, while the portal's
#   family is deduplicated (R-11, Appendix C-42). Each mobject joins at most one group
#   here (Var.owner), and add never closes a cycle.
OMITTED = ("Sector", "AnnularSector", "Annulus", "RoundedRectangle", "insert_n_curves")


@dataclasses.dataclass
class Var:
    name: str
    kind: str          # FLAT (a VMobject), SOLID (any other Mobject)
    group: bool = False
    owner: "Var | None" = None   # the one group holding it (see OMITTED: shared members)


class State:
    """What a program has defined so far, so every statement it emits is well typed."""

    def __init__(self, rng):
        self.rng = rng
        self.vars: list[Var] = []
        self.views: list[str] = []
        self.statements: list[str] = []

    def new_name(self):
        return f"v{len(self.vars)}"

    def pick(self, kind=None, group=None):
        pool = [v for v in self.vars
                if (kind is None or v.kind == kind) and (group is None or v.group == group)]
        return self.rng.choice(pool) if pool else None

    def emit(self, line):
        self.statements.append(line)


def construct(state):
    c = state.rng.choice(CONSTRUCTORS)
    var = Var(state.new_name(), c.kind)
    state.vars.append(var)
    state.emit(f"{var.name} = {c.cls}({c.args(state.rng)})")


def group(state):
    rng = state.rng
    free = [v for v in state.vars if v.owner is None]
    flat = [v for v in free if v.kind == FLAT]
    if len(flat) >= 2 and rng.random() < 0.7:
        members = rng.sample(flat, rng.randint(2, min(4, len(flat))))
        var = Var(state.new_name(), FLAT, group=True)
        state.emit(f"{var.name} = VGroup({', '.join(m.name for m in members)})")
    elif len(free) >= 2:
        members = rng.sample(free, rng.randint(2, min(4, len(free))))
        var = Var(state.new_name(), SOLID, group=True)
        state.emit(f"{var.name} = Group({', '.join(m.name for m in members)})")
    else:
        return construct(state)
    for member in members:
        member.owner = var
    state.vars.append(var)


def _ancestors(var):
    while var is not None:
        yield var
        var = var.owner


def positional(state):
    rng = state.rng
    v = state.pick()
    other = state.pick()
    choices = [
        lambda: f"{v.name}.shift({_vector(rng)})",
        lambda: f"{v.name}.scale({_num(rng, 0.3, 2.5)})",
        lambda: f"{v.name}.scale({_num(rng, 0.3, 2.5)}, about_edge={rng.choice(DIRECTIONS)})",
        lambda: f"{v.name}.scale({_num(rng, 0.3, 2.5)}, about_point={_point(rng)})",
        lambda: f"{v.name}.rotate({_num(rng, -3.0, 3.0)})",
        lambda: f"{v.name}.rotate({_num(rng, -3.0, 3.0)}, axis={rng.choice(AXES)})",
        lambda: f"{v.name}.rotate({_num(rng, -3.0, 3.0)}, about_point={_point(rng)})",
        lambda: f"{v.name}.stretch({_num(rng, 0.3, 2.5)}, {rng.randint(0, 1)})",
        lambda: f"{v.name}.flip({rng.choice(['UP', 'RIGHT'])})",
        lambda: f"{v.name}.move_to({_point(rng)})",
        lambda: f"{v.name}.move_to({other.name})",
        lambda: f"{v.name}.next_to({other.name}, {rng.choice(DIRECTIONS)}, buff={_num(rng, 0.0, 0.6)})",
        lambda: f"{v.name}.align_to({other.name}, {rng.choice(DIRECTIONS)})",
        lambda: f"{v.name}.to_edge({rng.choice(['UP', 'DOWN', 'LEFT', 'RIGHT'])})",
        lambda: f"{v.name}.to_corner({rng.choice(['UL', 'UR', 'DL', 'DR'])})",
        lambda: f"{v.name}.set_width({_num(rng, 0.3, 5.0)})",
        lambda: f"{v.name}.set_height({_num(rng, 0.3, 4.0)})",
        lambda: f"{v.name}.match_width({other.name})",
        lambda: f"{v.name}.match_height({other.name})",
        lambda: f"{v.name}.center()",
    ]
    state.emit(rng.choice(choices)())


def style(state):
    rng = state.rng
    v = state.pick()
    choices = [
        lambda: f"{v.name}.set_color({rng.choice(COLORS)})",
        lambda: f"{v.name}.set_opacity({_num(rng, 0.0, 1.0)})",
    ]
    if v.kind == FLAT:
        choices += [
            lambda: f"{v.name}.set_fill({rng.choice(COLORS)}, opacity={_num(rng, 0.0, 1.0)})",
            lambda: f"{v.name}.set_stroke({rng.choice(COLORS)}, width={_num(rng, 0.0, 8.0)})",
        ]
    state.emit(rng.choice(choices)())


def structure(state):
    rng = state.rng
    v = state.pick()
    roll = rng.random()
    if roll < 0.35:
        var = Var(state.new_name(), v.kind, v.group)
        state.vars.append(var)
        state.emit(f"{var.name} = {v.name}.copy()")
        return
    groups = [g for g in state.vars if g.group]
    if roll < 0.6 and groups:
        g = rng.choice(groups)
        free = [v for v in state.vars if v.owner is None and (g.kind != FLAT or v.kind == FLAT)
                and not any(v is a for a in _ancestors(g))]
        if free:
            member = rng.choice(free)
            member.owner = g
            state.emit(f"{g.name}.add({member.name})")
            return
    if roll < 0.7 and groups:
        g = rng.choice(groups)
        state.emit(f"{g.name}.arrange({rng.choice(DIRECTIONS)}, buff={_num(rng, 0.0, 0.6)})")
        return
    if roll < 0.75 and groups:
        g = rng.choice(groups)
        state.emit(f"{g.name}.arrange_in_grid(n_rows={rng.randint(1, 3)})")
        return
    flat = [x for x in state.vars if x.kind == FLAT and not x.group]
    if roll < 0.85 and len(flat) >= 2:
        a, b = rng.sample(flat, 2)
        state.emit(f"{a.name}.become({b.name})")
        return
    if flat:
        # The Reference requires matching point counts, so partial a copy of itself.
        a = rng.choice(flat)
        lo = _num(rng, 0.0, 0.5)
        state.emit(f"{a.name}.pointwise_become_partial({a.name}.copy(), {lo}, {_num(rng, lo + 0.1, 1.0)})")
        return
    positional(state)


def points(state):
    """Live point views: take one here; `edit_views` writes through it at the end."""
    rng = state.rng
    v = state.pick(FLAT, group=False)
    if v is None:
        return positional(state)
    if rng.random() < 0.5:
        name = f"p{len(state.views)}"
        state.views.append(name)
        state.emit(f"{name} = {v.name}.get_points()")
    else:
        state.emit(f"{v.name}.reverse_points()")


def scene(state):
    rng = state.rng
    v = state.pick()
    roll = rng.random()
    if roll < 0.3:
        others = rng.sample(state.vars, rng.randint(1, min(3, len(state.vars))))
        state.emit(f"self.add({', '.join(o.name for o in others)})")
    elif roll < 0.4:
        state.emit(f"self.remove({v.name})")
    elif roll < 0.75:
        chain = rng.choice([
            lambda: f"shift({_vector(rng)})",
            lambda: f"scale({_num(rng, 0.3, 2.0)})",
            lambda: f"rotate({_num(rng, -3.0, 3.0)})",
            lambda: f"set_color({rng.choice(COLORS)})",
            lambda: f"move_to({_point(rng)})",
        ])
        steps = ".".join(rng.choice([chain])() for _ in range(rng.randint(1, 3)))
        state.emit(f"self.play({v.name}.animate.{steps})")
    else:
        u = state.pick(v.kind)
        flat = v.kind == FLAT and u is not None and u.kind == FLAT and not v.group and not u.group
        options = [f"FadeIn({v.name})", f"FadeOut({v.name})", f"Rotate({v.name}, {_num(rng, -3.0, 3.0)})"]
        if flat:
            options += [f"Transform({v.name}, {u.name})", f"ShowCreation({v.name})",
                        f"ReplacementTransform({v.name}, {u.name}.copy())"]
        state.emit(f"self.play({rng.choice(options)})")


def edit_views(state):
    """In-place writes through the views taken earlier, after every other statement.

    Last, because the Reference's box ignores such a write (Appendix C-35, fixed
    under BN-07): a positional call after it would read a stale box there, and the
    difference would be that defect's, not the portal's. A final stale box is told
    apart in `compare` from the members' point boxes.
    """
    rng = state.rng
    for view in state.views:
        if rng.random() < 0.7:
            op = rng.choice([f"+= {_vector(rng)}", f"*= {_num(rng, 0.5, 1.5)}"])
            state.emit(f"{view} {op}")


KINDS = [(construct, 0.22), (group, 0.07), (positional, 0.28), (style, 0.1), (structure, 0.1),
         (points, 0.08), (scene, 0.15)]


def generate_program(seed: int, index: int, statements: int | None = None) -> dict:
    """Program `index` of the stream `seed`. The same pair always gives the same bytes."""
    rng = random.Random(f"fmn-api-fuzz/{GENERATOR_VERSION}/{seed}/{index}")
    state = State(rng)
    target = statements or rng.randint(4, 24)
    for _ in range(rng.randint(1, 3)):
        construct(state)
    while len(state.statements) < target:
        kind = rng.choices([k for k, _ in KINDS], weights=[w for _, w in KINDS])[0]
        kind(state)
    edit_views(state)
    return program_record(f"s{seed}-{index:06d}", state.statements, [v.name for v in state.vars],
                          seed=seed, index=index)


def program_record(program_id, statements, roots, **extra) -> dict:
    source = "".join(line + "\n" for line in statements)
    return {"id": program_id, "statements": list(statements), "source": source, "roots": list(roots),
            "digest": hashlib.sha256(source.encode()).hexdigest()[:16], **extra}


# ---------------------------------------------------------------------------
# Running both engines
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class Engines:
    portal_python: str
    reference_python: str
    reference_root: str
    timeout: int = 900
    program_timeout: int = 10
    # Filled from each batch's header line: what actually ran.
    identities: dict = dataclasses.field(default_factory=dict)
    # First programs whose end-of-batch replay measured differently (see run_both).
    position_mismatches: list = dataclasses.field(default_factory=list)


def reference_commit(root) -> str | None:
    try:
        return subprocess.run(["git", "-C", str(root), "rev-parse", "HEAD"], capture_output=True,
                              text=True, timeout=30, check=True).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return None


def _display_base():
    # One Xvfb display range per process, so parallel runs never collide.
    return 20000 + (os.getpid() % 2000) * 16


_DISPLAYS = iter(range(_display_base(), _display_base() + 1_000_000))


def run_batch(engines: Engines, programs: list[dict], work: pathlib.Path, engine: str) -> list[dict]:
    """One engine's facts for the batch `run_both` wrote to work/programs.ndjson, in order."""
    source = work / "programs.ndjson"
    facts = work / f"{engine}.ndjson"
    env = {"PATH": "/usr/bin:/bin", "HOME": os.environ.get("HOME", "/tmp"), **ONE_THREAD,
           "FMN_FUZZ_PROGRAMS": str(source), "FMN_FUZZ_FACTS": str(facts), "FMN_FUZZ_TOOLS": str(TOOLS),
           "FMN_FUZZ_TIMEOUT": str(engines.program_timeout)}
    if engine == "portal":
        # --construct-only runs the scene's whole lifecycle without rendering pixels,
        # and every portal version since 0.4 has it (planted negatives run old wheels).
        # The grammar has no updaters, so plays end where skip mode would put them.
        argv = [engines.portal_python, "-m", "fmn_python", "--construct-only", str(SCENE), "FuzzBatch"]
    else:
        env["PYTHONPATH"] = engines.reference_root
        argv = ["xvfb-run", "-n", str(next(_DISPLAYS)), "-s", "-screen 0 640x360x24",
                engines.reference_python, "-m", "manimlib", str(SCENE), "FuzzBatch", "-s", "-w",
                "-r", "320x180", "--video_dir", str(work / "reference_media")]
    log = work / f"{engine}.log"
    with open(log, "w", encoding="utf-8") as sink:
        try:
            code = subprocess.run(argv, cwd=work, env=env, stdout=sink, stderr=subprocess.STDOUT,
                                  timeout=engines.timeout).returncode
        except subprocess.TimeoutExpired:
            code = "timeout"
    records = {r["subject"]: r for r in sf.read_ndjson(facts)} if facts.exists() else {}
    if facts.exists():
        first = facts.read_text(encoding="utf-8").split("\n", 1)[0]
        if first.startswith("# "):
            identity = json.loads(first[2:])
            if engine == "reference":
                identity["commit"] = reference_commit(engines.reference_root)
            engines.identities[engine] = identity
    out = []
    for program in programs:
        record = records.get(program["id"])
        if record is None:
            # A batch that died part-way loses the rest; say so per program.
            record = {"subject": program["id"], "error": f"EngineExit: {engine} exited {code} before this program"}
        out.append(record)
    return out


REPLAY = "#replay"


def run_both(engines: Engines, programs: list[dict], work: pathlib.Path):
    """Both engines' facts for one batch, run side by side.

    The batch ends with a replay of its first program. A program's facts must not
    depend on what ran before it in the same scene, so a replay whose facts differ
    from the original's is counted in `engines.position_mismatches`."""
    work.mkdir(parents=True, exist_ok=True)
    batch = list(programs)
    if programs:
        batch.append({**programs[0], "id": programs[0]["id"] + REPLAY})
    (work / "programs.ndjson").write_text("".join(sf.canonical(p) + "\n" for p in batch), encoding="utf-8")
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        ref = pool.submit(run_batch, engines, batch, work, "reference")
        portal = pool.submit(run_batch, engines, batch, work, "portal")
        results = ref.result(), portal.result()
    if programs:
        for records in results:
            first, replay = ({key: value for key, value in record.items() if key != "subject"}
                             for record in (records[0], records[-1]))
            if first != replay and "error" not in first and "error" not in replay:
                engines.position_mismatches.append(programs[0]["id"])
    return tuple(records[:len(programs)] for records in results)


# ---------------------------------------------------------------------------
# Comparing
# ---------------------------------------------------------------------------


C35 = "c35-stale-reference-box"
C43 = "c43-stale-box-after-play"


def _normalize_stale_boxes(ref, portal):
    """Appendix C-35, member by member: where the Reference's box is not its own
    point box and the portal's box is that point box, the Reference was stale, and
    its point box is the value to compare. Anything else keeps `bbox_points` on both
    sides, so a box that is not its point box in both engines by design compares
    equal, and a stale portal box stays a difference (BN-07: the box follows the
    points)."""
    if "members" not in ref or "members" not in portal:
        return ref, portal, 0
    portal_boxes = {m["path"]: m for m in portal["members"]}
    count = 0
    members = []
    for member in ref["members"]:
        fresh = member.get("bbox_points")
        other = portal_boxes.get(member["path"])
        if (fresh is not None and other is not None and "bbox_points" not in other
                and sf._within_tolerance(fresh, other.get("bbox"))):
            member = {key: value for key, value in member.items() if key != "bbox_points"}
            member["bbox"] = fresh
            # Circle.get_radius measures from get_center, which reads the same stale
            # box: recompute it from the member's first point and the point box.
            getters = member.get("getters")
            points = member.get("points")
            if isinstance(getters, dict) and "get_radius" in getters and isinstance(points, list) and points:
                start, centre = points[0], fresh[1]
                radius = round(sum((a - b) ** 2 for a, b in zip(start, centre)) ** 0.5)
                member["getters"] = {**getters, "get_radius": radius}
            count += 1
        members.append(member)
    return {**ref, "members": members}, portal, count


def compare(ref: dict, portal: dict, exclusions) -> dict:
    """The shared structural diff, plus the fuzzer's own facts: which roots exist and
    the order of the scene list (by root index; -1 for an unnamed member).

    A program an engine never measured (its process died first) is `engine-exit`,
    never equal, whatever the other engine did."""
    exited = [engine for engine, record in (("reference", ref), ("portal", portal))
              if str(record.get("error", "")).startswith("EngineExit")]
    if exited:
        return {"schema": sf.DIFF_SCHEMA, "version": sf.VERSION, "subject": ref.get("subject"),
                "verdict": "engine-exit", "difference_count": 0, "first_difference": None,
                "differences": [], "excluded": {}, "envelope_violations": [], "exited": exited}
    ref, portal, normalized = _normalize_stale_boxes(ref, portal)
    result = sf.diff_subject(ref, portal, exclusions, ignore=frozenset())
    if normalized:
        result["excluded"][C35] = normalized
        if result["verdict"] == "equal":
            result["verdict"] = "equal-with-exclusions"
    extra = []
    if "error" not in ref and "error" not in portal:
        for key in ("absent_roots", "scene"):
            if ref.get(key) != portal.get(key):
                extra.append({"path": "", "class": None, "fact": key,
                              "reference": ref.get(key), "portal": portal.get(key)})
        # Appendix C-43: a stale box right after a play is the Reference's defect, never
        # the portal's, whose box follows the points.
        if ref.get("stale_after_play"):
            result["excluded"][C43] = result["excluded"].get(C43, 0) + ref["stale_after_play"]
            if result["verdict"] == "equal":
                result["verdict"] = "equal-with-exclusions"
        if portal.get("stale_after_play"):
            extra.append({"path": "", "class": None, "fact": "stale_after_play",
                          "reference": ref.get("stale_after_play"), "portal": portal["stale_after_play"]})
    if extra:
        result["differences"] = extra + result["differences"]
        result["first_difference"] = result["differences"][0]
        result["difference_count"] += len(extra)
        result["verdict"] = "differs"
    return result


def signature(result: dict):
    """What a minimization must preserve: the first difference's fact, and for an error
    its two exception classes."""
    first = result.get("first_difference")
    if not first:
        return None
    if first["fact"] == "error":
        return ("error", str(first["reference"]).split(":")[0], str(first["portal"]).split(":")[0])
    return (first["fact"].split(".")[0], first.get("class"))


def load_exclusions(path=EXCLUSIONS):
    return sf.in_scope(sf.load_exclusions(path), SCOPE)


# ---------------------------------------------------------------------------
# Minimizing
# ---------------------------------------------------------------------------


def ddmin(items, reproduces, budget=400):
    """Zeller's ddmin over a list. `reproduces` takes a list of candidate lists and
    returns one bool per candidate, so a whole round runs as one batch. The result
    always reproduces (the input is assumed to), and the loop ends within `budget`
    predicate evaluations."""
    items = list(items)
    n = 2
    spent = 0
    while len(items) >= 2 and spent < budget:
        size = max(1, len(items) // n)
        chunks = [items[i:i + size] for i in range(0, len(items), size)]
        complements = [[x for j, c in enumerate(chunks) if j != i for x in c] for i in range(len(chunks))]
        candidates = chunks + complements
        verdicts = reproduces(candidates)
        spent += len(candidates)
        hit = next((c for c, ok in zip(candidates, verdicts) if ok and len(c) < len(items)), None)
        if hit is not None:
            n = 2 if hit in chunks else max(n - 1, 2)
            items = hit
            continue
        if n >= len(items):
            break
        n = min(len(items), n * 2)
    return items


def minimize_program(engines: Engines, program: dict, target, exclusions, work: pathlib.Path,
                     budget: int = 400) -> dict:
    """The smallest statement subset of `program` that still yields `target`."""
    rounds = iter(range(1_000_000))

    def reproduces(candidates):
        batch = [program_record(f"{program['id']}-m{k}", [program["statements"][i] for i in idx],
                                program["roots"]) for k, idx in enumerate(candidates)]
        refs, portals = run_both(engines, batch, work / f"round{next(rounds)}")
        return [signature(compare(r, p, exclusions)) == target for r, p in zip(refs, portals)]

    keep = ddmin(range(len(program["statements"])), reproduces, budget)
    statements = [program["statements"][i] for i in keep]
    names = set()
    for line in statements:
        names.update(token for token in _tokens(line) if token in program["roots"])
    roots = [name for name in program["roots"] if name in names]
    return program_record(f"{program['id']}-min", statements, roots or program["roots"])


def _tokens(line):
    word = ""
    for ch in line + " ":
        if ch.isalnum() or ch == "_":
            word += ch
        else:
            if word:
                yield word
            word = ""


def regression_test(program: dict, result: dict) -> str:
    """A pytest body that replays the minimized program in the portal."""
    first = result["first_difference"]
    body = "".join(f"    {line}\n" for line in program["statements"])
    return (f"def test_api_fuzz_{program['digest']}():\n"
            f"    # api_differential_fuzz: {first['fact']} at member {first['path']!r}\n"
            f"    # Reference: {sf.canonical(first['reference'])[:160]}\n"
            f"    # portal:    {sf.canonical(first['portal'])[:160]}\n"
            f"{body}")


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------


def command_generate(args):
    out = pathlib.Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    programs = [generate_program(args.seed, i) for i in range(args.count)]
    (out / "programs.ndjson").write_text("".join(sf.canonical(p) + "\n" for p in programs), encoding="utf-8")
    (out / "meta.json").write_text(sf.canonical({"seed": args.seed, "count": args.count,
                                                 "generator_version": GENERATOR_VERSION}) + "\n")
    return programs


def command_run(args, engines, programs=None):
    out = pathlib.Path(args.out)
    programs = programs or sf.read_ndjson(out / "programs.ndjson")
    batches = [programs[i:i + args.batch] for i in range(0, len(programs), args.batch)]
    refs, portals = {}, {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, args.jobs // 2)) as pool:
        jobs = {pool.submit(run_both, engines, batch, out / "batches" / f"b{k:05d}"): batch
                for k, batch in enumerate(batches)}
        for job in concurrent.futures.as_completed(jobs):
            ref, portal = job.result()
            refs.update((r["subject"], r) for r in ref)
            portals.update((r["subject"], r) for r in portal)
    for engine, records in (("reference", refs), ("portal", portals)):
        (out / f"{engine}.ndjson").write_text(
            "".join(sf.canonical(records[p["id"]]) + "\n" for p in programs), encoding="utf-8")
    return refs, portals


def command_diff(args, programs=None, refs=None, portals=None):
    out = pathlib.Path(args.out)
    programs = programs or sf.read_ndjson(out / "programs.ndjson")
    refs = refs or {r["subject"]: r for r in sf.read_ndjson(out / "reference.ndjson")}
    portals = portals or {r["subject"]: r for r in sf.read_ndjson(out / "portal.ndjson")}
    exclusions = load_exclusions(args.exclusions)
    verdicts = []
    for program in programs:
        result = compare(refs[program["id"]], portals[program["id"]], exclusions)
        verdicts.append({"program": program, "result": result})
    return verdicts, refs, portals


def engine_exits(programs, refs, portals) -> int:
    """Programs that an engine never measured because its process died first."""
    return sum(1 for program in programs
               if any(str(records[program["id"]].get("error", "")).startswith("EngineExit")
                      for records in (refs, portals)))


def log_line(program, result, engines_id, minimized=None, outcome=None):
    first = result.get("first_difference")
    return sf.canonical({
        "schema": LOG_SCHEMA, "version": LOG_VERSION,
        "seed": program.get("seed"), "index": program.get("index"), "program": program["id"],
        "digest": program["digest"], "statements": len(program["statements"]),
        "engines": engines_id, "outcome": outcome or result["verdict"],
        "first_difference": None if not first else {"path": first["path"], "class": first.get("class"),
                                                    "fact": first["fact"]},
        "excluded_by": sorted(result.get("excluded", {})),
        "minimized": None if minimized is None else {"digest": minimized["digest"],
                                                     "statements": len(minimized["statements"])},
    })


def engine_ids(engines: Engines) -> dict:
    return {engine: {"interpreter": path, **engines.identities.get(engine, {})}
            for engine, path in (("portal", engines.portal_python), ("reference", engines.reference_python))}


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("campaign", "generate"):
        p = sub.add_parser(name)
        p.add_argument("--seed", type=int, required=True)
        p.add_argument("--count", type=int, required=True)
        p.add_argument("--out", required=True)
    for name in ("run", "diff", "minimize"):
        p = sub.add_parser(name)
        p.add_argument("out")
    for p in sub.choices.values():
        p.add_argument("--jobs", type=int, default=8)
        p.add_argument("--batch", type=int, default=50)
        p.add_argument("--limit", type=int, default=20, help="programs to minimize")
        p.add_argument("--budget", type=int, default=400, help="ddmin evaluations per program")
        p.add_argument("--exclusions", default=str(EXCLUSIONS))
        p.add_argument("--timeout", type=int, default=900, help="seconds per engine batch")
        p.add_argument("--match", default="*", help="minimize only first differences whose fact matches")
        p.add_argument("--match-class", default="*", help="... and whose member class matches")
        p.add_argument("--portal-python", default=os.environ.get("FMN_FUZZ_PORTAL_PYTHON"),
                       help="a Python with the portal wheel installed (or FMN_FUZZ_PORTAL_PYTHON)")
        p.add_argument("--reference-python", default=os.environ.get("FMN_FUZZ_REFERENCE_PYTHON"),
                       help="the pinned Reference's venv Python (or FMN_FUZZ_REFERENCE_PYTHON)")
        p.add_argument("--reference-root", default=str(ROOT / "scripts" / "manim_ref"))
    args = parser.parse_args(argv)
    if args.command != "generate" and args.command != "diff" and not (
            args.portal_python and args.reference_python):
        parser.error(f"{args.command} needs --portal-python and --reference-python")
    # Each engine runs with its batch directory as cwd, so every path is absolute.
    args.out = str(pathlib.Path(args.out).resolve())
    engines = Engines(args.portal_python, args.reference_python, args.reference_root, args.timeout)
    out = pathlib.Path(args.out)

    if args.command == "generate":
        command_generate(args)
        return 0
    programs = command_generate(args) if args.command == "campaign" else sf.read_ndjson(out / "programs.ndjson")
    started = time.monotonic()
    refs = portals = None
    if args.command in ("campaign", "run"):
        refs, portals = command_run(args, engines, programs)
        if args.command == "run":
            return 0
    verdicts, refs, portals = command_diff(args, programs, refs, portals)
    differing = [v for v in verdicts if v["result"]["verdict"] == "differs"]
    minimized = {}
    if args.command in ("campaign", "minimize"):
        exclusions = load_exclusions(args.exclusions)
        seen, chosen = set(), []
        (out / "minimized").mkdir(parents=True, exist_ok=True)
        for verdict in differing:
            if len(chosen) >= args.limit:
                break
            target = signature(verdict["result"])
            first = verdict["result"]["first_difference"]
            if target in seen or not (fnmatch.fnmatchcase(first["fact"], args.match)
                                      and fnmatch.fnmatchcase(str(first.get("class")), args.match_class)):
                continue  # one minimization per distinct first difference
            seen.add(target)
            chosen.append((verdict["program"], target))

        def minimize_one(job):
            # Each program's ddmin rounds are sequential; different programs run side by side.
            program, target = job
            small = minimize_program(engines, program, target, exclusions,
                                     out / "minimize" / program["id"], args.budget)
            refs_m, portals_m = run_both(engines, [small], out / "minimize" / program["id"] / "final")
            result = compare(refs_m[0], portals_m[0], exclusions)
            (out / "minimized" / f"{program['id']}.py").write_text(regression_test(small, result))
            return program["id"], small

        with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, args.jobs // 2)) as pool:
            minimized.update(pool.map(minimize_one, chosen))
    with open(out / "log.ndjson", "w", encoding="utf-8") as log:
        for verdict in verdicts:
            program = verdict["program"]
            log.write(log_line(program, verdict["result"], engine_ids(engines),
                               minimized.get(program["id"])) + "\n")
    counts = {}
    for verdict in verdicts:
        counts[verdict["result"]["verdict"]] = counts.get(verdict["result"]["verdict"], 0) + 1
    summary = {"schema": LOG_SCHEMA, "version": LOG_VERSION, "programs": len(verdicts),
               "verdicts": dict(sorted(counts.items())), "minimized": len(minimized),
               "engine_exits": engine_exits(programs, refs, portals),
               "position_mismatches": len(engines.position_mismatches),
               "seconds": round(time.monotonic() - started, 1)}
    (out / "summary.json").write_text(sf.canonical(summary) + "\n")
    print(sf.canonical(summary))
    return 0


if __name__ == "__main__":
    sys.exit(main())
