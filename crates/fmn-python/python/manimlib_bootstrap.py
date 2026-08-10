"""The pure-Python skin over FrankenManim's narrow PyO3 engine seam.

This file is embedded in the extension module.  It deliberately contains the
ordinary Python object-model pieces which CPython implements better than an FFI
type can: cooperative ``__init__``, mutable live containers, copy/deepcopy and
pickle state, and schema-driven module/class construction.
"""

from __future__ import annotations

import ast as _ast
import copy as _copy
import enum as _enum
import importlib as _importlib
import inspect as _inspect
import math as _math
import sys as _sys
import types as _types
import weakref as _weakref


class _LiveSubmobjects(list):
    """A list whose accepted mutations are mirrored into Marionette."""

    def __init__(self, owner):
        super().__init__()
        self._owner_ref = _weakref.ref(owner)

    def _commit(self, candidate):
        candidate = list(candidate)
        owner = self._owner_ref()
        if owner is None:
            raise ReferenceError("the owning Mobject has been collected")
        owner._replace_submobjects(candidate)
        list.clear(self)
        list.extend(self, candidate)

    def append(self, value):
        self._commit([*self, value])

    def extend(self, values):
        self._commit([*self, *list(values)])

    def insert(self, index, value):
        candidate = list(self)
        candidate.insert(index, value)
        self._commit(candidate)

    def clear(self):
        self._commit([])

    def pop(self, index=-1):
        candidate = list(self)
        value = candidate.pop(index)
        self._commit(candidate)
        return value

    def remove(self, value):
        candidate = list(self)
        candidate.remove(value)
        self._commit(candidate)

    def reverse(self):
        candidate = list(self)
        candidate.reverse()
        self._commit(candidate)

    def sort(self, *args, **kwargs):
        candidate = list(self)
        candidate.sort(*args, **kwargs)
        self._commit(candidate)

    def __setitem__(self, index, value):
        candidate = list(self)
        candidate[index] = value
        self._commit(candidate)

    def __delitem__(self, index):
        candidate = list(self)
        del candidate[index]
        self._commit(candidate)

    def __iadd__(self, values):
        self.extend(values)
        return self

    def __imul__(self, count):
        candidate = list(self)
        candidate *= count
        self._commit(candidate)
        return self


class _LiveUniforms(dict):
    """Typed engine uniforms plus an open-ended Python extension map."""

    def __init__(self, owner):
        super().__init__()
        self._owner_ref = _weakref.ref(owner)
        self._extras = {"opacity": 1.0}

    def _owner(self):
        owner = self._owner_ref()
        if owner is None:
            raise ReferenceError("the owning Mobject has been collected")
        return owner

    def __getitem__(self, key):
        owner = self._owner()
        if key in owner._uniform_names():
            return owner._get_uniform(key)
        return self._extras[key]

    def __setitem__(self, key, value):
        owner = self._owner()
        if key in owner._uniform_names():
            owner._set_uniform(key, value)
        else:
            self._extras[key] = value

    def __delitem__(self, key):
        if key in self._owner()._uniform_names():
            raise TypeError(f"engine uniform {key!r} cannot be deleted")
        del self._extras[key]

    def __iter__(self):
        yield from self._owner()._uniform_names()
        yield from self._extras

    def __len__(self):
        return len(self._owner()._uniform_names()) + len(self._extras)

    def __contains__(self, key):
        return key in self._owner()._uniform_names() or key in self._extras

    def get(self, key, default=None):
        try:
            return self[key]
        except KeyError:
            return default

    def keys(self):
        return dict.fromkeys(iter(self)).keys()

    def items(self):
        return [(key, self[key]) for key in self]

    def values(self):
        return [self[key] for key in self]

    def update(self, other=(), /, **kwargs):
        incoming = dict(other, **kwargs)
        for key, value in incoming.items():
            self[key] = value

    def copy(self):
        return dict(self.items())

    def __repr__(self):
        return repr(dict(self.items()))

    def __eq__(self, other):
        try:
            return dict(self.items()) == dict(other)
        except (TypeError, ValueError):
            return False


def _install_live_state(mobject):
    mobject.submobjects = _LiveSubmobjects(mobject)
    mobject.uniforms = _LiveUniforms(mobject)
    mobject.updaters = []


def _family_preorder(root):
    result = []
    seen = set()
    visiting = set()

    def visit(mobject):
        marker = id(mobject)
        if marker in visiting:
            raise _FamilyCycleError("submobjects would create a family cycle")
        if marker in seen:
            return
        if not isinstance(mobject, _BridgeMobject):
            raise TypeError("submobjects must be Mobject instances")
        visiting.add(marker)
        seen.add(marker)
        result.append(mobject)
        for child in mobject.submobjects:
            visit(child)
        visiting.remove(marker)

    visit(root)
    return result


def _copy_mobject_graph(root, deep, memo=None):
    if memo is None:
        memo = {}
    existing = memo.get(id(root))
    if existing is not None:
        return existing

    bound_shells = root._copy_family_shells()
    if bound_shells is None:
        pairs = []
        for old in _family_preorder(root):
            new = type(old).__new__(type(old))
            new._restore_engine_state(old._engine_state())
            pairs.append((old, new))
    else:
        pairs = list(bound_shells)

    mapping = {old: new for old, new in pairs}
    for old, new in pairs:
        memo[id(old)] = new
        _install_live_state(new)

    internal = {"submobjects", "uniforms", "updaters", "_scene"}
    for old, new in pairs:
        mapped_children = [mapping[child] for child in old.submobjects]
        new.submobjects.extend(mapped_children)

        attributes = {
            key: value
            for key, value in old.__dict__.items()
            if key not in internal
        }
        if deep:
            attributes = _copy.deepcopy(attributes, memo)
        else:
            attributes = {
                key: mapping.get(value, value)
                if isinstance(value, _BridgeMobject)
                else value
                for key, value in attributes.items()
            }
        new.__dict__.update(attributes)

        extras = old.uniforms._extras
        new.uniforms._extras = (
            _copy.deepcopy(extras, memo) if deep else dict(extras)
        )
        # Function objects remain shared even under deepcopy, while the list is
        # independent, matching manim's updater copy rule.
        new.updaters = list(old.updaters)

    return mapping[root]


def _restore_mobject(cls, engine_state, attributes, children, extras, updaters):
    result = cls.__new__(cls)
    result._restore_engine_state(engine_state)
    _install_live_state(result)
    result.__dict__.update(attributes)
    result.uniforms._extras = extras
    result.updaters = updaters
    result.submobjects.extend(children)
    return result


class Mobject(_BridgeMobject):
    data_dtype = [("point", 3), ("rgba", 4)]
    aligned_data_keys = ["point"]
    pointlike_data_keys = ["point"]

    def __init__(self, *submobjects, **kwargs):
        _install_live_state(self)
        for key, value in kwargs.items():
            setattr(self, key, value)
        self._engine_init()
        if submobjects:
            self.submobjects.extend(submobjects)

    def init_data(self):
        pass

    def init_points(self):
        pass

    def init_uniforms(self):
        if not isinstance(getattr(self, "uniforms", None), _LiveUniforms):
            self.uniforms = _LiveUniforms(self)

    @property
    def data(self):
        return self._data_array()

    @property
    def points(self):
        return self.data["point"]

    def add(self, *mobjects):
        self.submobjects.extend(mobjects)
        return self

    # The Reference's container protocol (manimlib/mobject/mobject.py):
    # a mobject iterates, indexes, and measures as its submobject list;
    # slicing regroups through the family's group class.
    def split(self):
        return self.submobjects

    def get_group_class(self):
        return getattr(_FMN_MODULE, "Group", Mobject)

    def __getitem__(self, value):
        if isinstance(value, slice):
            return self.get_group_class()(*self.split()[value])
        return self.split()[value]

    def __iter__(self):
        return iter(self.split())

    def __len__(self):
        return len(self.split())

    def remove(self, *mobjects):
        for mobject in mobjects:
            if mobject in self.submobjects:
                self.submobjects.remove(mobject)
        return self

    def add_updater(self, updater, index=None, call=True):
        if index is None:
            self.updaters.append(updater)
        else:
            self.updaters.insert(index, updater)
        if call:
            self._dispatch_updater(updater, 0.0)
        return self

    def remove_updater(self, updater):
        self.updaters = [item for item in self.updaters if item is not updater]
        return self

    def clear_updaters(self, recurse=True):
        targets = _family_preorder(self) if recurse else [self]
        for target in targets:
            target.updaters.clear()
        return self

    def _dispatch_updater(self, updater, dt):
        try:
            signature = _inspect.signature(updater)
            positional = [
                parameter
                for parameter in signature.parameters.values()
                if parameter.kind
                in (
                    parameter.POSITIONAL_ONLY,
                    parameter.POSITIONAL_OR_KEYWORD,
                )
            ]
            variadic = any(
                parameter.kind is parameter.VAR_POSITIONAL
                for parameter in signature.parameters.values()
            )
        except (TypeError, ValueError):
            positional = [None, None]
            variadic = True
        if variadic or len(positional) >= 2:
            return updater(self, dt)
        return updater(self)

    def copy(self, deep=False):
        return _copy_mobject_graph(self, bool(deep), {})

    def __copy__(self):
        return _copy_mobject_graph(self, False, {})

    def __deepcopy__(self, memo):
        return _copy_mobject_graph(self, True, memo)

    def __reduce_ex__(self, protocol):
        del protocol
        internal = {"submobjects", "uniforms", "updaters", "_scene"}
        attributes = {
            key: value
            for key, value in self.__dict__.items()
            if key not in internal
        }
        return (
            _restore_mobject,
            (
                type(self),
                self._engine_state(),
                attributes,
                list(self.submobjects),
                dict(self.uniforms._extras),
                list(self.updaters),
            ),
        )


class VMobject(Mobject):
    data_dtype = [
        ("point", 3),
        ("stroke_rgba", 4),
        ("stroke_width", 1),
        ("joint_angle", 1),
        ("fill_rgba", 4),
        ("base_normal", 3),
        ("fill_border_width", 1),
    ]

    def get_group_class(self):
        return getattr(_FMN_MODULE, "VGroup", VMobject)


class Scene(_SceneCore):
    def __init__(self, *args, **kwargs):
        self.args = args
        self.kwargs = kwargs

    def setup(self):
        pass

    def construct(self):
        pass

    def tear_down(self):
        pass

    @property
    def mobjects(self):
        return self._engine_roots()

    def run(self):
        return self._run_lifecycle()

    render = run

    @staticmethod
    def _dispatch_updater_batch(mobjects, dt):
        # fm-zoi rung 1: one native→Python crossing per frame. Iterates the
        # engine's target list in order and snapshots each mobject's updater
        # list at that mobject's turn — identical ordering and observable
        # state to rung 0's per-updater dispatch.
        for mobject in mobjects:
            for updater in list(mobject.updaters):
                mobject._dispatch_updater(updater, dt)


class InteractiveScene(Scene):
    def embed(self, namespace=None):
        return _portal_embed(self, namespace)

    def checkpoint_paste(self):
        return _portal_checkpoint_paste(self)


class Animation:
    def __init__(self, mobject=None, run_time=1.0, rate_func=None, **kwargs):
        self.mobject = mobject
        self.run_time = run_time
        self.rate_func = rate_func if rate_func is not None else (lambda alpha: alpha)
        self.__dict__.update(kwargs)

    def begin(self):
        pass

    def finish(self):
        pass

    def interpolate(self, alpha):
        self.interpolate_mobject(self.rate_func(alpha))
        return self

    def interpolate_mobject(self, alpha):
        del alpha

    def copy(self):
        return _copy.copy(self)


def _portal_embed(scene=None, namespace=None):
    """Enter IPython with an explicit scene/namespace capability boundary."""

    try:
        ipython = _importlib.import_module("IPython")
    except ImportError as error:
        raise _CapabilityError(
            "IPython is not installed; use checkpoint_paste(scene) for a "
            "dependency-free Studio handoff"
        ) from error
    user_ns = {} if namespace is None else dict(namespace)
    if scene is not None:
        user_ns.setdefault("scene", scene)
    return ipython.embed(user_ns=user_ns)


def _portal_checkpoint_paste(scene):
    """Return the canonical SceneState bytes consumed by Studio checkpoints."""

    if not isinstance(scene, _SceneCore):
        raise TypeError("checkpoint_paste expects a Scene")
    return scene._checkpoint_bytes()


def _placeholder_function(module_name, name):
    def unavailable(*args, **kwargs):
        del args, kwargs
        raise NotImplementedError(
            f"{module_name}.{name} is present in the parity surface but its "
            "semantic binding has not landed"
        )

    unavailable.__name__ = name
    unavailable.__qualname__ = name
    unavailable.__module__ = module_name
    return unavailable


def _placeholder_method(module_name, owner, name):
    def unavailable(self, *args, **kwargs):
        del self, args, kwargs
        raise NotImplementedError(
            f"{module_name}.{owner}.{name} is present in the parity surface "
            "but its semantic binding has not landed"
        )

    unavailable.__name__ = name
    unavailable.__qualname__ = f"{owner}.{name}"
    unavailable.__module__ = module_name
    return unavailable


def _surface_init(self, *args, **kwargs):
    self.args = args
    self.__dict__.update(kwargs)


class _UnavailablePygletWindow:
    """Import-compatible base for the deliberately absent pyglet gateway."""

    def __init__(self, *args, **kwargs):
        del args, kwargs
        raise _CapabilityError(
            "the Reference's PygletWindow gateway is unavailable; "
            "FrankenManim Studio owns interactive windows"
        )


_UnavailablePygletWindow.__name__ = "PygletWindow"
_UnavailablePygletWindow.__qualname__ = "PygletWindow"
_UnavailablePygletWindow.__module__ = "manimlib.window"


def _constant(detail):
    if detail == "-":
        return None
    if detail == "True":
        return True
    if detail == "False":
        return False
    try:
        return int(detail)
    except ValueError:
        pass
    try:
        return float(detail)
    except ValueError:
        pass
    # Materialize the ledger's literal forms. The direction/coordinate
    # constants MUST be real NumPy arrays — the Reference's are, and the
    # corpus multiplies and adds them (`0.25 * DOWN`); a string here
    # turns every corpus import into a TypeError.
    if detail.startswith("np.array(") and detail.endswith(")"):
        numpy = _importlib.import_module("numpy")
        return numpy.array(_ast.literal_eval(detail[len("np.array(") : -1]))
    if detail == "np.pi":
        return _math.pi
    try:
        # Quoted strings, tuples, dicts, and other pure literals.
        return _ast.literal_eval(detail)
    except (ValueError, SyntaxError):
        # Symbolic defaults (config references, derived expressions) keep
        # their declared spelling until the constants environment lands.
        return detail


def _schema_rows():
    rows = []
    in_symbols = False
    for raw_line in _API_SCHEMA_TSV.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            in_symbols = line == "[symbols]"
            continue
        if not in_symbols:
            continue
        columns = raw_line.split("\t", 5)
        if len(columns) == 6:
            rows.append(tuple(columns))
    return rows


def _ensure_module(name):
    existing = _sys.modules.get(name)
    if existing is not None:
        return existing
    module = _types.ModuleType(name)
    module.__package__ = name
    module.__path__ = []
    _sys.modules[name] = module
    if "." in name:
        parent_name, child_name = name.rsplit(".", 1)
        parent = _ensure_module(parent_name)
        setattr(parent, child_name, module)
    return module


def _base_names(detail):
    result = []
    for raw in detail.split(","):
        name = raw.strip().split("[", 1)[0].split(".")[-1]
        if not name or name in {"-", "object", "ABC", "Generic", "Protocol"}:
            continue
        result.append(name)
    return result


def _pinned_manim_config():
    """The Reference's default_config.yml at the pin, as namespaces.

    Only the sections the ledger's symbolic constant defaults consult;
    values are the pinned file's, verbatim (fm-d3gt).
    """
    ns = _types.SimpleNamespace
    return ns(
        camera=ns(resolution=(1920, 1080), fps=30, background_color="#333333", background_opacity=1.0),
        sizes=ns(
            frame_height=8.0,
            small_buff=0.1,
            med_small_buff=0.25,
            med_large_buff=0.5,
            large_buff=1.0,
            default_mobject_to_edge_buff=0.5,
            default_mobject_to_mobject_buff=0.25,
        ),
        key_bindings=ns(
            pan_3d="d", pan="f", reset="r", quit="q", select="s", unselect="u",
            grab="g", x_grab="h", y_grab="v", z_grab="z", resize="t", color="c",
            information="i", cursor="k",
        ),
        colors=ns(
            blue_e="#1C758A", blue_d="#29ABCA", blue_c="#58C4DD", blue_b="#9CDCEB", blue_a="#C7E9F1",
            teal_e="#49A88F", teal_d="#55C1A7", teal_c="#5CD0B3", teal_b="#76DDC0", teal_a="#ACEAD7",
            green_e="#699C52", green_d="#77B05D", green_c="#83C167", green_b="#A6CF8C", green_a="#C9E2AE",
            yellow_e="#E8C11C", yellow_d="#F4D345", yellow_c="#FFFF00", yellow_b="#FFEA94", yellow_a="#FFF1B6",
            gold_e="#C78D46", gold_d="#E1A158", gold_c="#F0AC5F", gold_b="#F9B775", gold_a="#F7C797",
            red_e="#CF5044", red_d="#E65A4C", red_c="#FC6255", red_b="#FF8080", red_a="#F7A1A3",
            maroon_e="#94424F", maroon_d="#A24D61", maroon_c="#C55F73", maroon_b="#EC92AB", maroon_a="#ECABC1",
            purple_e="#644172", purple_d="#715582", purple_c="#9A72AC", purple_b="#B189C6", purple_a="#CAA3E8",
            grey_e="#222222", grey_d="#444444", grey_c="#888888", grey_b="#BBBBBB", grey_a="#DDDDDD",
            white="#FFFFFF", black="#000000", grey_brown="#736357", dark_brown="#8B4513",
            light_brown="#CD853F", pink="#D147BD", light_pink="#DC75CD", green_screen="#00FF00",
            orange="#FF862F", pure_red="#FF0000", pure_green="#00FF00", pure_blue="#0000FF",
        ),
        vmobject=ns(default_stroke_width=4.0, default_stroke_color="#DDDDDD", default_fill_color="#888888"),
        mobject=ns(default_mobject_color="#FFFFFF", default_light_color="#BBBBBB"),
        text=ns(font="Consolas", alignment="LEFT", font_size_for_unit_height=144),
        tex=ns(template="default", font_size_for_unit_height=144),
    )


def _resolve_symbolic_constants(rows):
    """Fixpoint-evaluate the ledger's symbolic constant defaults.

    The corpus does arithmetic on these at import time (FRAME_WIDTH - 1,
    FRAME_Y_RADIUS * DOWN), so the derived spellings must become real
    values. Literals resolve through _constant; symbolic spellings
    evaluate in a closed environment seeded with the pinned
    manim_config, NumPy, and every constant already resolved — repeated
    until no pass makes progress. Spellings that never resolve (runtime
    machinery like EventDispatcher()) keep their declared string, the
    pre-existing behavior.
    """
    numpy = _importlib.import_module("numpy")
    env = {
        "manim_config": _pinned_manim_config(),
        "np": numpy,
        "version": lambda _name: "1.7.2",
    }
    pending = {}
    resolved = {}
    for module_name, qualified, kind, _arity, detail, *_rest in rows:
        if kind != "constant" or "." in qualified:
            continue
        value = _constant(detail)
        if isinstance(value, str) and value == detail and detail != "-":
            pending[(module_name, qualified)] = detail
        else:
            resolved[(module_name, qualified)] = value
            env.setdefault(qualified, value)
    progressed = True
    while progressed and pending:
        progressed = False
        for key in list(pending):
            detail = pending[key]
            try:
                # The spelling being evaluated is a row of the committed
                # docs/api/ledger.tsv — the same repo-controlled schema
                # this whole surface is synthesized from, never runtime
                # input — with no builtins and a closed environment.
                value = eval(detail, {"__builtins__": {}}, env)  # noqa: S307
            except Exception:
                continue
            resolved[key] = value
            env.setdefault(key[1], value)
            del pending[key]
            progressed = True
    for key, detail in pending.items():
        resolved[key] = detail
    return resolved


def _install_schema_surface():
    root = _FMN_MODULE
    _sys.modules.setdefault(__name__, root)
    root.__path__ = []
    rows = _schema_rows()
    for module_name, *_ in rows:
        _ensure_module(module_name)
    _resolved_constants = _resolve_symbolic_constants(rows)

    specials = {
        ("manimlib.mobject.mobject", "Mobject"): Mobject,
        ("manimlib.mobject.types.vectorized_mobject", "VMobject"): VMobject,
        ("manimlib.scene.scene", "Scene"): Scene,
        ("manimlib.scene.interactive_scene", "InteractiveScene"): InteractiveScene,
        ("manimlib.animation.animation", "Animation"): Animation,
    }
    pyglet_module = _ensure_module("manimlib.window")
    pyglet_module.PygletWindow = _UnavailablePygletWindow
    classes_by_name = {
        "Enum": _enum.Enum,
        "Exception": Exception,
        "PygletWindow": _UnavailablePygletWindow,
    }
    for (module_name, name), cls in specials.items():
        cls.__module__ = module_name
        setattr(_ensure_module(module_name), name, cls)
        classes_by_name.setdefault(name, cls)

    class_rows = [
        row for row in rows if row[2] == "class" and "." not in row[1]
    ]
    pending = [
        row for row in class_rows if (row[0], row[1]) not in specials
    ]
    while pending:
        progress = False
        for row in list(pending):
            module_name, name, _kind, _origin, _exported, detail = row
            names = _base_names(detail)
            if any(base not in classes_by_name for base in names):
                continue
            bases = tuple(classes_by_name[base] for base in names) or (object,)
            attributes = {"__module__": module_name}
            if not any(
                isinstance(base, type)
                and issubclass(base, (Mobject, Scene, Animation))
                for base in bases
            ):
                attributes["__init__"] = _surface_init
            try:
                if bases == (_enum.Enum,):
                    cls = _enum.Enum(name, {}, module=module_name)
                else:
                    cls = type(name, bases, attributes)
            except TypeError as error:
                base_names = ", ".join(base.__name__ for base in bases)
                raise RuntimeError(
                    f"API schema MRO for {module_name}.{name} "
                    f"cannot preserve bases ({base_names})"
                ) from error
            setattr(_ensure_module(module_name), name, cls)
            classes_by_name.setdefault(name, cls)
            pending.remove(row)
            progress = True
        if progress:
            continue
        unresolved = "; ".join(
            f"{module_name}.{name}: {_base_names(detail)}"
            for module_name, name, _kind, _origin, _exported, detail in pending
        )
        raise RuntimeError(f"unresolved API schema class bases: {unresolved}")

    lifecycle = {
        "__init__",
        "init_data",
        "init_points",
        "init_uniforms",
        "setup",
        "construct",
        "tear_down",
        "begin",
        "finish",
        "interpolate",
        "interpolate_mobject",
    }
    for module_name, qualified, kind, _origin, _exported, _detail in rows:
        module = _ensure_module(module_name)
        if kind == "method" and "." in qualified:
            owner_name, method_name = qualified.rsplit(".", 1)
            owner = getattr(module, owner_name, None)
            if (
                isinstance(owner, type)
                and method_name not in lifecycle
                and not hasattr(owner, method_name)
            ):
                setattr(
                    owner,
                    method_name,
                    _placeholder_method(module_name, owner_name, method_name),
                )
        elif kind == "function" and "." not in qualified:
            if not hasattr(module, qualified):
                setattr(
                    module,
                    qualified,
                    _placeholder_function(module_name, qualified),
                )
        elif kind == "constant" and "." not in qualified:
            if not hasattr(module, qualified):
                setattr(
                    module,
                    qualified,
                    _resolved_constants.get(
                        (module_name, qualified), _constant(_detail)
                    ),
                )
        elif kind == "leaked_import" and "." not in qualified:
            if hasattr(module, qualified):
                continue
            try:
                origin = _importlib.import_module(_origin)
                value = getattr(origin, qualified, origin)
            except (ImportError, ValueError):
                value = _placeholder_function(module_name, qualified)
            setattr(module, qualified, value)

    in_canonical = False
    for raw_line in _API_OVERLAY_TSV.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            in_canonical = line == "[canonical]"
            continue
        if not in_canonical:
            continue
        columns = raw_line.split("\t")
        if len(columns) < 2 or ":" not in columns[0]:
            continue
        reference, canonical = columns[0], columns[1]
        module_name, qualified = reference.split(":", 1)
        module = _ensure_module(module_name)
        if "." in qualified:
            owner_name, reference_name = qualified.rsplit(".", 1)
            owner = getattr(module, owner_name, None)
            if isinstance(owner, type):
                value = getattr(
                    owner,
                    reference_name,
                    _placeholder_method(module_name, owner_name, reference_name),
                )
                setattr(owner, reference_name, value)
                setattr(owner, canonical, value)
        elif hasattr(module, qualified):
            value = getattr(module, qualified)
            setattr(module, canonical, value)

    for module_name, name, _kind, _origin, exported, _detail in rows:
        if exported != "1" or "." in name:
            continue
        module = _ensure_module(module_name)
        if hasattr(module, name):
            setattr(root, name, getattr(module, name))

    root.__version__ = "0.1.0"
    root.__reference_commit__ = (
        "6199a00d4c1b1127ebe45cb629c3f22538b10e13"
    )
    exception_module = _types.ModuleType("manimlib.exceptions")
    exception_module.StaleHandleError = _StaleHandleError
    exception_module.ForeignStageError = _ForeignStageError
    exception_module.FamilyCycleError = _FamilyCycleError
    exception_module.CapabilityError = _CapabilityError
    _sys.modules["manimlib.exceptions"] = exception_module
    root._exceptions = exception_module

    expected_exports = {
        name
        for _module, name, _kind, _origin, exported, _detail in rows
        if exported == "1" and "." not in name
    }
    for name in list(root.__dict__):
        if not name.startswith("_") and name not in expected_exports:
            del root.__dict__[name]
    # The pinned Reference intentionally has no root __all__; wildcard import
    # therefore exposes every non-underscore name in the assembled namespace.
    root.__dict__.pop("__all__", None)


def _install_rate_functions():
    """The Reference's pure easing formulas, verbatim (fm-d3gt).

    manimlib/utils/rate_functions.py at the Reference pin is pure
    arithmetic; the corpus calls these at import time (custom/** builds
    squished rate functions at module scope), so they must be real
    callables before the placeholder pass runs. Engine-side easing stays
    fmn-anim's; these are the API-semantic Python definitions.
    """
    _math_local = _math

    def _bezier(points):
        degree = len(points) - 1

        def result(t):
            return sum(
                ((1 - t) ** (degree - k))
                * (t**k)
                * _math_local.comb(degree, k)
                * point
                for k, point in enumerate(points)
            )

        return result

    def linear(t):
        return t

    def smooth(t):
        s = 1 - t
        return (t**3) * (10 * s * s + 5 * s * t + t * t)

    def rush_into(t):
        return 2 * smooth(0.5 * t)

    def rush_from(t):
        return 2 * smooth(0.5 * (t + 1)) - 1

    def slow_into(t):
        return _math_local.sqrt(1 - (1 - t) * (1 - t))

    def double_smooth(t):
        if t < 0.5:
            return 0.5 * smooth(2 * t)
        return 0.5 * (1 + smooth(2 * t - 1))

    def there_and_back(t):
        new_t = 2 * t if t < 0.5 else 2 * (1 - t)
        return smooth(new_t)

    def there_and_back_with_pause(t, pause_ratio=1.0 / 3):
        a = 2.0 / (1.0 - pause_ratio)
        if t < 0.5 - pause_ratio / 2:
            return smooth(a * t)
        if t < 0.5 + pause_ratio / 2:
            return 1
        return smooth(a - a * t)

    def running_start(t, pull_factor=-0.5):
        return _bezier([0, 0, pull_factor, pull_factor, 1, 1, 1])(t)

    def overshoot(t, pull_factor=1.5):
        return _bezier([0, 0, pull_factor, pull_factor, 1, 1])(t)

    def not_quite_there(func=smooth, proportion=0.7):
        def result(t):
            return proportion * func(t)

        return result

    def wiggle(t, wiggles=2):
        return there_and_back(t) * _math_local.sin(wiggles * _math_local.pi * t)

    def squish_rate_func(func, a=0.4, b=0.6):
        def result(t):
            if a == b:
                return a
            if t < a:
                return func(0)
            if t > b:
                return func(1)
            return func((t - a) / (b - a))

        return result

    def lingering(t):
        return squish_rate_func(lambda t: t, 0, 0.8)(t)

    def exponential_decay(t, half_life=0.1):
        return 1 - _math_local.exp(-t / half_life)

    functions = {
        "linear": linear,
        "smooth": smooth,
        "rush_into": rush_into,
        "rush_from": rush_from,
        "slow_into": slow_into,
        "double_smooth": double_smooth,
        "there_and_back": there_and_back,
        "there_and_back_with_pause": there_and_back_with_pause,
        "running_start": running_start,
        "overshoot": overshoot,
        "not_quite_there": not_quite_there,
        "wiggle": wiggle,
        "squish_rate_func": squish_rate_func,
        "lingering": lingering,
        "exponential_decay": exponential_decay,
    }
    module = _ensure_module("manimlib.utils.rate_functions")
    for name, function in functions.items():
        setattr(module, name, function)
        if not hasattr(_FMN_MODULE, name):
            setattr(_FMN_MODULE, name, function)


_install_rate_functions()


_install_schema_surface()
