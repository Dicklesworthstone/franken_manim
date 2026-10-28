"""Every portal callable binds calls exactly as the pinned Reference signature does.

The one API schema (API_SCHEMA.tsv [params]) is the authority for the Python
front door's signatures (plan §16.2, fm-schema-codegen-front-doors-r8-z206).
For every Reference callable in the compatibility claim (status neither
``excluded`` nor ``tiered``), the portal object must be call-compatible: each
call the Reference signature accepts binds to the same parameters with the
same defaults. Concretely:

- named parameters keep their names (or the [param_canonical] spelling), their
  kinds, and their positional order; ``*args``/``**kwargs`` may be renamed,
  since no caller can address them by name;
- the portal may add parameters only after the Reference's positional ones,
  each with a default (and none at all before a Reference ``*args``);
- a parameter the Reference requires may gain a default; a Reference default
  may not be dropped;
- defaults are compared as values in the portal's own namespace. A Reference
  mutable default (a list, dict, set, or array) may become ``None`` or an equal
  tuple, the portal's shared-mutable-default idiom; a lambda must take the
  same parameters and use the same names and constants.

Every other divergence must be ruled, owner by owner, in API_OVERLAY.tsv
[signature_rulings], and a ruling with no divergence left fails as stale.
"""
import ast
import builtins
import importlib
import inspect
import operator

import numpy as np

_KINDS = {
    "positional_only": inspect.Parameter.POSITIONAL_ONLY,
    "positional_or_keyword": inspect.Parameter.POSITIONAL_OR_KEYWORD,
    "var_positional": inspect.Parameter.VAR_POSITIONAL,
    "keyword_only": inspect.Parameter.KEYWORD_ONLY,
    "var_keyword": inspect.Parameter.VAR_KEYWORD,
}
_POSITIONAL = (inspect.Parameter.POSITIONAL_ONLY, inspect.Parameter.POSITIONAL_OR_KEYWORD)
_VARIADIC = (inspect.Parameter.VAR_POSITIONAL, inspect.Parameter.VAR_KEYWORD)
_EMPTY = inspect.Parameter.empty


def _sections(text):
    sections, current = {}, None
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            current = sections.setdefault(line[1:-1], [])
            continue
        if current is not None:
            current.append(raw.split("\t"))
    return sections


def load_surface(m):
    schema, overlay = _sections(m._API_SCHEMA_TSV), _sections(m._API_OVERLAY_TSV)
    params = {}
    for owner, ordinal, name, kind, _annotation, default in schema["params"]:
        params.setdefault(owner, []).append((int(ordinal), name, kind, None if default == "-" else default))
    status = {row[0]: row[1] for row in overlay["status"]}
    renames = {(row[0], row[1]): row[2] for row in overlay["param_canonical"]}
    rulings = {row[0]: (row[1], row[2]) for row in overlay.get("signature_rulings", [])}
    return {owner: sorted(rows) for owner, rows in params.items()}, status, renames, rulings


def claimed(owner, status):
    """Excluded and tiered surfaces are outside the compatibility claim."""
    module, _, qual = owner.partition(":")
    parts = qual.split(".")
    for depth in range(len(parts), 0, -1):
        found = status.get(f"{module}:{'.'.join(parts[:depth])}")
        if found is not None:
            return found not in ("excluded", "tiered")
    return True


def resolve(owner):
    module_name, _, qual = owner.partition(":")
    module = importlib.import_module(module_name)
    obj = module
    for part in qual.split("."):
        obj = inspect.getattr_static(obj, part) if inspect.isclass(obj) else getattr(obj, part)
    if isinstance(obj, (staticmethod, classmethod)):
        obj = obj.__func__
    if isinstance(obj, property):
        obj = obj.fget  # the Reference spells these as decorated functions too
    return obj, module


_ARITHMETIC = {ast.Add: operator.add, ast.Sub: operator.sub, ast.Mult: operator.mul,
               ast.Div: operator.truediv}


class _ReferenceLambda:
    """A Reference lambda default, matched against the portal's by its
    parameters and the names and constants its body uses."""

    def __init__(self, node):
        self.params = [arg.arg for arg in node.args.args]
        body = list(ast.walk(node.body))
        self.names = sorted({n.id for n in body if isinstance(n, ast.Name)} - set(self.params))
        self.constants = sorted(repr(n.value) for n in body if isinstance(n, ast.Constant))

    def matches(self, portal):
        code = getattr(portal, "__code__", None)
        if getattr(portal, "__name__", None) != "<lambda>" or code is None:
            return False
        constants = sorted(repr(c) for c in code.co_consts if c is not None)
        return (list(code.co_varnames[:code.co_argcount]) == self.params
                and sorted(code.co_names) == self.names and constants == self.constants)


def evaluate_default(text, namespace):
    """A Reference default spelling's value in the portal's namespace: names,
    attributes, calls, containers and arithmetic, never arbitrary code."""

    def value(node):
        if isinstance(node, ast.Constant):
            return node.value
        if isinstance(node, ast.Name):
            # The Reference module's own names first, then Python's (dict, bool).
            return namespace[node.id] if node.id in namespace else getattr(builtins, node.id)
        if isinstance(node, ast.Attribute):
            return getattr(value(node.value), node.attr)
        if isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.USub):
            return -value(node.operand)
        if isinstance(node, ast.BinOp) and type(node.op) in _ARITHMETIC:
            return _ARITHMETIC[type(node.op)](value(node.left), value(node.right))
        if isinstance(node, ast.List):
            return [value(item) for item in node.elts]
        if isinstance(node, ast.Tuple):
            return tuple(value(item) for item in node.elts)
        if isinstance(node, ast.Dict):
            return {value(key): value(item) for key, item in zip(node.keys, node.values)}
        if isinstance(node, ast.Call):
            return value(node.func)(*(value(arg) for arg in node.args),
                                    **{kw.arg: value(kw.value) for kw in node.keywords})
        if isinstance(node, ast.Lambda):
            return _ReferenceLambda(node)
        raise ValueError(f"unsupported default syntax {type(node).__name__}")

    return value(ast.parse(text, mode="eval").body)


def _mutable(value):
    return isinstance(value, (list, dict, set, np.ndarray))


def _same_default(reference, portal):
    if reference is portal:
        return True
    if isinstance(reference, _ReferenceLambda):
        return reference.matches(portal)
    if _mutable(reference):
        if portal is None:
            return True
        if isinstance(reference, np.ndarray) or isinstance(portal, np.ndarray):
            return np.shape(reference) == np.shape(portal) and bool(np.array_equal(reference, portal))
        if isinstance(reference, list) and isinstance(portal, tuple):
            return list(portal) == reference
        return type(reference) is type(portal) and reference == portal
    if isinstance(reference, bool) or isinstance(portal, bool):
        return type(reference) is type(portal) and reference == portal
    if isinstance(reference, (int, float)) and isinstance(portal, (int, float)):
        return reference == portal
    if isinstance(reference, str) and isinstance(portal, str):
        return str(reference) == str(portal)
    if callable(reference) or callable(portal):
        return False  # functions and classes must be the same object
    try:
        return type(reference) is type(portal) and bool(reference == portal)
    except Exception:
        return False


def divergences(owner, reference, portal_params, namespace, renames):
    """Call-compatibility violations of one portal signature, as strings."""
    problems = []
    portal = list(portal_params)
    by_name = {p.name: p for p in portal}
    positional = [p for p in portal if p.kind in _POSITIONAL]
    ref_positional = [row for row in reference if _KINDS[row[2]] in _POSITIONAL]
    ref_has_varargs = any(_KINDS[row[2]] is inspect.Parameter.VAR_POSITIONAL for row in reference)

    for kind in _VARIADIC:
        if any(_KINDS[row[2]] is kind for row in reference) and not any(p.kind is kind for p in portal):
            problems.append(f"missing {'*args' if kind is inspect.Parameter.VAR_POSITIONAL else '**kwargs'}")

    for index, (_, name, kind_text, default) in enumerate(reference):
        kind = _KINDS[kind_text]
        if kind in _VARIADIC:
            continue
        spelled = renames.get((owner, name), name)
        parameter = by_name.get(spelled) or by_name.get(name)
        if parameter is None:
            problems.append(f"{name}: missing")
            continue
        method_receiver = index == 0 and name in ("self", "cls")
        if not method_receiver:
            if kind is inspect.Parameter.POSITIONAL_OR_KEYWORD and parameter.kind is not kind:
                problems.append(f"{name}: kind {parameter.kind.name.lower()}, not positional_or_keyword")
            elif kind is inspect.Parameter.KEYWORD_ONLY and parameter.kind not in (
                    inspect.Parameter.KEYWORD_ONLY, inspect.Parameter.POSITIONAL_OR_KEYWORD):
                problems.append(f"{name}: kind {parameter.kind.name.lower()}, not keyword_only")
            elif kind is inspect.Parameter.POSITIONAL_ONLY and parameter.kind not in _POSITIONAL:
                problems.append(f"{name}: kind {parameter.kind.name.lower()}, not positional_only")
        if default is None:
            continue
        if parameter.default is _EMPTY:
            problems.append(f"{name}: no default (Reference {default})")
            continue
        try:
            value = evaluate_default(default, namespace)
        except Exception as error:
            problems.append(f"{name}: Reference default {default} unevaluable: {type(error).__name__}")
            continue
        if not _same_default(value, parameter.default):
            problems.append(f"{name}: default {parameter.default!r}, Reference {default}")

    for index, (_, name, _kind, _default) in enumerate(ref_positional):
        spelled = renames.get((owner, name), name)
        if index >= len(positional) or positional[index].name not in (spelled, name):
            found = positional[index].name if index < len(positional) else None
            problems.append(f"positional {index}: {found}, Reference {name}")
            break
    for extra in positional[len(ref_positional):]:
        if ref_has_varargs:
            problems.append(f"{extra.name}: extra positional before the Reference *args")
        elif extra.default is _EMPTY:
            problems.append(f"{extra.name}: extra positional without a default")
    known = {renames.get((owner, row[1]), row[1]) for row in reference} | {row[1] for row in reference}
    for parameter in portal:
        if (parameter.kind is inspect.Parameter.KEYWORD_ONLY and parameter.name not in known
                and parameter.default is _EMPTY):
            problems.append(f"{parameter.name}: extra keyword-only without a default")
    return problems


def census(m):
    """``(owner, problems)`` for every claimed Reference callable, plus counts."""
    import manimlib

    params, status, renames, _rulings = load_surface(m)
    shared = {**vars(manimlib), "np": np}
    rows, checked = [], 0
    for owner, reference in params.items():
        if not claimed(owner, status):
            continue
        checked += 1
        try:
            obj, module = resolve(owner)
        except Exception as error:
            rows.append((owner, [f"unresolved: {type(error).__name__}"]))
            continue
        try:
            signature = inspect.signature(obj)
        except (TypeError, ValueError) as error:
            rows.append((owner, [f"no signature: {error}"[:120]]))
            continue
        problems = divergences(owner, reference, signature.parameters.values(),
                               {**shared, **vars(module)}, renames)
        if problems:
            rows.append((owner, problems))
    return rows, checked


def verdict(m):
    """``(checked, unruled, stale)`` for the live portal against its schema."""
    _params, _status, _renames, rulings = load_surface(m)
    rows, checked = census(m)
    found = {owner for owner, _ in rows}
    unruled = [f"{owner}: {'; '.join(problems)}" for owner, problems in rows if owner not in rulings]
    return checked, unruled, sorted(set(rulings) - found), len(found)


def run_signature_conformance(m):
    checked, unruled, stale, ruled = verdict(m)
    assert checked >= 1200, checked
    assert not unruled, f"{len(unruled)} portal signatures diverge from the schema without a ruling:\n" + "\n".join(unruled)
    assert not stale, f"[signature_rulings] rows whose divergence is gone: {stale}"
    return {"checked": checked, "ruled": ruled}


ARC = "manimlib.mobject.geometry:Arc.__init__"

# Geometry classes the Reference constructs with no arguments, in the order the
# both-doors e2e scenario builds their Rust counterparts from the generated
# reference_defaults.rs constants.
GEOMETRY_DEFAULTS = (
    "Arc", "Circle", "Dot", "SmallDot", "Ellipse", "AnnularSector", "Sector", "Annulus",
    "Elbow", "Rectangle", "Square", "RoundedRectangle", "RegularPolygon", "Triangle",
    "Line", "DashedLine", "Vector", "ArrowTip",
)


def geometry_default_points(m):
    """``[(name, points)]``: each class built through the portal with no
    arguments, as its family's points in preorder (Reference get_all_points)."""
    from manimlib.mobject import geometry

    del m
    return [(name, [[float(value) for value in point]
                    for point in getattr(geometry, name)().get_all_points()])
            for name in GEOMETRY_DEFAULTS]


def run_planted_divergences(m):
    """Each drift the check exists for fails it: a portal default, a schema
    default, a removed parameter, and a ruling left behind."""
    from manimlib.mobject.geometry import Arc

    init = Arc.__init__
    defaults = init.__defaults__
    with_defaults = [p.name for p in inspect.signature(init).parameters.values()
                     if p.default is not _EMPTY and p.kind in _POSITIONAL]
    index = with_defaults.index("radius")
    assert defaults[index] == 1.0, defaults
    init.__defaults__ = defaults[:index] + (2.0,) + defaults[index + 1:]
    try:
        _checked, unruled, _stale, _ruled = verdict(m)
    finally:
        init.__defaults__ = defaults
    assert [row for row in unruled if row.startswith(ARC + ": radius: default 2.0")], unruled

    schema = m._API_SCHEMA_TSV
    row = f"{ARC}\t3\tradius\tpositional_or_keyword\tfloat\t1.0\n"
    assert schema.count(row) == 1
    try:
        m._API_SCHEMA_TSV = schema.replace(row, row.replace("\t1.0\n", "\t1.5\n"))
        _checked, unruled, _stale, _ruled = verdict(m)
        assert [line for line in unruled if line.startswith(ARC + ": radius: default 1.0, Reference 1.5")], unruled
        m._API_SCHEMA_TSV = schema.replace(row, row.replace("\tradius\t", "\tarc_radius\t"))
        _checked, unruled, _stale, _ruled = verdict(m)
        assert [line for line in unruled if line.startswith(ARC + ": arc_radius: missing")], unruled
    finally:
        m._API_SCHEMA_TSV = schema

    overlay = m._API_OVERLAY_TSV
    try:
        m._API_OVERLAY_TSV = overlay.replace(
            "[signature_rulings]\n", f"[signature_rulings]\n{ARC}\tbaseline\tplanted\n", 1)
        _checked, _unruled, stale, _ruled = verdict(m)
        assert stale == [ARC], stale
    finally:
        m._API_OVERLAY_TSV = overlay
    _checked, unruled, stale, _ruled = verdict(m)
    assert not unruled and not stale, (unruled, stale)
    return 4
