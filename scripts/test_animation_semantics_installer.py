"""Test the shipped installer against real bootstrap classes without CPython FFI.

The bootstrap's animation definitions are compiled unchanged. Only its Rust
Mobject storage boundary is replaced by a small NumPy-backed fixture. These are
Python contract tests, not native renderer or installed-wheel evidence.
"""
from __future__ import annotations

import ast
import collections.abc
import copy
import importlib.util
import math
import types
import unittest
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[1]
PYTHON = ROOT / "crates/fmn-python/python"
BOOTSTRAP = PYTHON / "manimlib_bootstrap.py"
INSTALLER = PYTHON / "manimlib/_animation_semantics.py"
ANIMATION_CLASSES = {
    "Animation", "_NativeAnimation", "Transform", "ReplacementTransform",
    "TransformFromCopy", "CyclicReplace", "Swap", "AnimationGroup",
    "LaggedStart", "Succession", "LaggedStartMap",
}


def make_native():
    class Mobject:
        pointlike_data_keys = ("point",)
        const_data_keys = ()

        def __init__(self, *children, points=((0.0, 0.0, 0.0),)):
            self.submobjects = list(children)
            self.data = np.zeros(len(points), dtype=[("point", "f8", 3), ("rgba", "f8", 4)])
            self.data["point"] = np.asarray(points).reshape((-1, 3))
            self.uniforms = {"shading": (0.0, 0.0, 0.0)}
            self.locked_data_keys = set()
            self.locked_uniform_keys = set()
            self.animating = False
            self.suspended = False
            self.updaters = []
            self.changes = 0

        def __iter__(self):
            return iter(self.submobjects)

        def copy(self):
            return copy.deepcopy(self)

        def get_family(self):
            return [self, *(descendant for child in self for descendant in child.get_family())]

        def family_members_with_points(self):
            return [member for member in self.get_family() if len(member.data)]

        def set_animating_status(self, value):
            for member in self.get_family():
                member.animating = value

        def _is_updating_suspended(self):
            return self.suspended

        def suspend_updating(self):
            self.suspended = True

        def resume_updating(self):
            self.suspended = False

        def is_aligned_with(self, other):
            return [len(m.data) for m in self.get_family()] == [len(m.data) for m in other.get_family()]

        def align_data_and_family(self, other):
            if not self.is_aligned_with(other):
                raise AssertionError("fixture deliberately does not simulate Rust alignment")

        def has_updaters(self):
            return any(m.updaters for m in self.get_family())

        def lock_matching_data(self, start, target):
            for live, left, right in zip(self.get_family(), start.get_family(), target.get_family()):
                live.locked_data_keys = {
                    key for key in live.data.dtype.names
                    if np.array_equal(left.data[key], right.data[key])
                }

        def unlock_data(self):
            for member in self.get_family():
                member.locked_data_keys.clear()
                member.locked_uniform_keys.clear()

        def note_changed_data(self):
            self.changes += 1

        def update(self, dt):
            if not self.suspended:
                for updater in self.updaters:
                    updater(self, dt)

    def refuse(name, parameters):
        refused = [key for key, active in parameters if active]
        if refused:
            raise NotImplementedError(f"{name}: {', '.join(refused)}")

    native = types.ModuleType("manimlib")
    g = vars(native)
    g.update({
        "_np": np, "_copy": copy, "_math": math,
        "_collections_abc": collections.abc, "_OUT": (0.0, 0.0, 1.0),
        "_vec3": lambda value: tuple(value), "_refuse_unrouted": refuse,
        "_interpolate": lambda left, right, alpha: (1.0 - alpha) * left + alpha * right,
        "_smooth_rate": lambda value: value * value * (3.0 - 2.0 * value),
        "Mobject": Mobject, "_AnimationBuilder": type("_AnimationBuilder", (), {}),
        "_FMN_ROOT": native,
    })
    native.straight_path = g["_interpolate"]
    source = ast.parse(BOOTSTRAP.read_text(encoding="utf-8"), filename=str(BOOTSTRAP))
    selected = [node for node in source.body if isinstance(node, ast.ClassDef) and node.name in ANIMATION_CLASSES]
    if {node.name for node in selected} != ANIMATION_CLASSES:
        raise AssertionError("bootstrap animation class inventory changed")
    exec(compile(ast.Module(body=selected, type_ignores=[]), str(BOOTSTRAP), "exec"), g)
    spec = importlib.util.spec_from_file_location("fmn_installer_under_test", INSTALLER)
    installer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(installer)
    installer.install(native)
    return native


class NativeCompositionContractTests(unittest.TestCase):
    def setUp(self):
        self.native = make_native()

    def test_compositions_preserve_native_timing_and_deferred_root(self):
        n = self.native
        first, second = n.Animation(n.Mobject()), n.Animation(n.Mobject())
        for cls, lag in ((n.AnimationGroup, 0.0), (n.LaggedStart, 0.05), (n.Succession, 1.0)):
            with self.subTest(cls=cls.__name__):
                group = cls(first, second)
                self.assertIsNone(group.mobject)
                self.assertIsNone(group.run_time)
                self.assertIsNone(group.rate_func)
                self.assertEqual(group.lag_ratio, lag)
                self.assertEqual(group.animations, [first, second])
                self.assertEqual(str(group), cls.__name__)

    def test_iterable_and_nested_compositions_remain_native_specs(self):
        n = self.native
        children = [n.Animation(n.Mobject()), n.Animation(n.Mobject())]
        nested = n.AnimationGroup((child for child in children), run_time=5.0)
        outer = n.Succession(nested, n.LaggedStart(*children))
        self.assertEqual(nested.animations, children)
        self.assertEqual(nested.run_time, 5.0)
        self.assertIs(outer.animations[0], nested)

    def test_lagged_start_map_can_construct_its_native_group(self):
        n = self.native
        members = n.Mobject(n.Mobject(), n.Mobject())
        mapped = n.LaggedStartMap(n.Animation, members)
        self.assertEqual([child.mobject for child in mapped.animations], list(members))
        self.assertEqual(mapped.run_time, 2.0)
        self.assertEqual(mapped.lag_ratio, 0.05)

    def test_only_real_composition_types_accept_a_missing_root(self):
        n = self.native
        class PretendGroup(n._NativeAnimation):
            _native_kind = "animation_group"
        for cls in (n.Animation, n._NativeAnimation, n.Transform, PretendGroup):
            with self.subTest(cls=cls.__name__), self.assertRaises(TypeError):
                cls(None)
        with self.assertRaises(TypeError):
            n._NativeAnimation(object())

    def test_targetless_transforms_do_not_invent_a_target(self):
        n = self.native
        for cls in (n.CyclicReplace, n.Swap):
            with self.subTest(cls=cls.__name__):
                animation = cls(n.Mobject(), n.Mobject())
                self.assertIsNone(animation._native_target())
                self.assertFalse(hasattr(animation, "target_mobject"))

    def test_ordinary_transforms_still_resolve_and_validate_targets(self):
        n = self.native
        target = n.Mobject()
        self.assertIs(n.Transform(n.Mobject(), target)._native_target(), target)
        with self.assertRaises(TypeError):
            n.Transform(n.Mobject(), object())._native_target()


if __name__ == "__main__":
    unittest.main()
