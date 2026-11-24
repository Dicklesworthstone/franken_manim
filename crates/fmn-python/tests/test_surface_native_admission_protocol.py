"""Surface route admission over explicit metadata/storage doubles.

These tests exercise the shipping predicate and invocation scope. Native grid
alignment, writable-view admission, pixels and team counters are covered by
surface_alignment.py against the installed extension.
"""
from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
import sys
import unittest
from weakref import ref

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from fmn_python.surface_alignment import install_surface_alignment


def environment():
    queries = []

    class LiveChildren(list):
        pass

    class Mobject:
        def __init__(self, *children):
            self.submobjects = LiveChildren(children)
            self.native_shape = None
            self.native_points = False
            self.schema = "records"

        @property
        def data(self):
            raise AssertionError("admission exported record data")

        def _data_array(self, *args):
            raise AssertionError("admission exported a record view")

        def _has_points(self):
            return self.native_points

        def n_records(self):
            raise AssertionError("admission materialized placement")

        def field_names(self):
            raise AssertionError("admission materialized placement")

        def get_family(self):
            return [self, *(member for child in self.submobjects for member in child.get_family())]

        def has_points(self):
            return self._has_points()

        def align_points(self, other):
            return self

        def is_aligned_with(self, other):
            return True

    class Surface(Mobject):
        def __init__(self, shape=(3, 4), *children):
            super().__init__(*children)
            self.native_shape = shape
            self.native_points = shape is not None
            self.schema = "surface" if shape is not None else "records"
            if shape is not None:
                self.resolution = shape
                self.compute_triangle_indices()

        def compute_triangle_indices(self):
            rows, cols = self.resolution
            self.triangle_indices = np.zeros(6 * (rows - 1) * (cols - 1), dtype=int)
            return self.triangle_indices

        def get_triangle_indices(self):
            return self.triangle_indices

    class Animation:
        pass

    class Transform(Animation):
        _native_kind = "transform"

        def __init__(self, source, target):
            self.mobject, self.target_mobject = source, target
            self.previous_refusal = False

    class Composition(Animation):
        def __init__(self, *animations):
            self.animations = animations

    class Builder:
        def __init__(self, animation):
            self.animation, self.builds = animation, 0

        def build(self):
            self.builds += 1
            return self.animation

    class Scene:
        before_admission = None
        fail = False

        def play(self, *animations, **kwargs):
            if self.before_admission is not None:
                self.before_admission()
            if self.fail:
                raise RuntimeError("authored failure")
            def classify(animation):
                if isinstance(animation, Composition):
                    return tuple(classify(child) for child in animation.animations)
                return native._requires_python_animation(animation)
            return tuple(classify(animation) for animation in animations)

    def grid_resolution(member):
        queries.append(("grid", member))
        if member.native_shape == "invalid":
            raise ValueError("invalid native grid")
        return member.native_shape

    def compatible(left, right):
        queries.append(("schema", left, right))
        return left.schema == right.schema

    native = SimpleNamespace(
        Mobject=Mobject, Surface=Surface, Transform=Transform, Animation=Animation,
        Scene=Scene, _AnimationBuilder=Builder, _LiveSubmobjects=LiveChildren, _np=np,
        _surface_grid_resolution=grid_resolution, _align_surface_grids=lambda *args: None,
        _become_schema_compatible=compatible,
        _requires_python_animation=lambda animation: getattr(animation, "previous_refusal", False),
        prepare_animation=lambda value: value.build() if isinstance(value, Builder) else value,
        Composition=Composition, queries=queries,
    )
    install_surface_alignment(native)
    return native


class SurfaceNativeAdmissionTests(unittest.TestCase):
    def setUp(self):
        self.m = environment()

    def pair(self, shape=(3, 4)):
        return self.m.Surface(shape), self.m.Surface(shape)

    def classify(self, left, right):
        return self.m.Scene().play(self.m.Transform(left, right))[0]

    def test_one_stock_surface_transform_uses_no_record_exports(self):
        left, right = self.pair()
        before = left.triangle_indices
        before[:] = np.arange(len(before))[::-1]
        self.assertFalse(self.classify(left, right))
        self.assertIs(left.triangle_indices, before)
        self.assertTrue(self.m.queries)

    def test_nested_surface_group_and_empty_roots_keep_native_route(self):
        left, right = self.pair()
        a = self.m.Surface(None, self.m.Mobject(left))
        b = self.m.Surface(None, self.m.Mobject(right))
        self.assertFalse(self.classify(a, b))

    def test_unequal_recursive_structure_is_not_flattened_into_correspondence(self):
        a, b = self.pair()
        left = self.m.Mobject(self.m.Mobject(a), self.m.Mobject())
        right = self.m.Mobject(self.m.Mobject(), self.m.Mobject(b))
        self.assertTrue(self.classify(left, right))

    def test_shared_family_alias_correspondence_must_match(self):
        child, peer = self.pair()
        shared = self.m.Mobject(child, child)
        same_aliases = self.m.Mobject(peer, peer)
        separate = self.m.Mobject(self.m.Surface(), self.m.Surface())
        self.assertFalse(self.classify(shared, same_aliases))
        self.assertTrue(self.classify(shared, separate))
        self.assertTrue(self.classify(separate, shared))

    def test_equal_record_counts_with_different_grid_shapes_stay_callback(self):
        self.assertTrue(self.classify(self.m.Surface((2, 6)), self.m.Surface((3, 4))))

    def test_different_schemas_stay_callback(self):
        left, right = self.pair()
        right.schema = "custom"
        self.assertTrue(self.classify(left, right))

    def test_unstructured_meshes_stay_callback(self):
        left, right = self.pair()
        left.native_shape = right.native_shape = None
        self.assertTrue(self.classify(left, right))

    def test_missing_or_stale_python_metadata_stays_callback(self):
        for edit in (lambda member: vars(member).pop("triangle_indices"),
                     lambda member: setattr(member, "resolution", (4, 3)),
                     lambda member: setattr(member, "triangle_indices", np.zeros(3, dtype=int))):
            with self.subTest(edit=edit):
                left, right = self.pair()
                edit(left)
                self.assertTrue(self.classify(left, right))

    def test_authored_resolution_iterable_is_not_evaluated(self):
        class AuthoredShape:
            def __iter__(self):
                raise AssertionError("authored resolution iterator ran during admission")
        left, right = self.pair()
        left.resolution = AuthoredShape()
        self.assertTrue(self.classify(left, right))

    def test_data_property_override_is_not_called_during_admission(self):
        class ObservedSurface(self.m.Surface):
            @property
            def data(self):
                raise AssertionError("authored data property ran during admission")
        self.assertTrue(self.classify(ObservedSurface(), self.m.Surface()))

    def test_authored_topology_descriptors_are_not_bypassed(self):
        for name in ("resolution", "triangle_indices"):
            with self.subTest(name=name):
                class ObservedSurface(self.m.Surface):
                    pass
                left, right = ObservedSurface(), self.m.Surface()
                def observe(member):
                    raise AssertionError("authored topology descriptor ran during admission")
                setattr(ObservedSurface, name, property(observe))
                self.assertTrue(self.classify(left, right))

    def test_instance_storage_override_is_preserved(self):
        for name in ("_data_array", "_has_points", "n_records", "field_names"):
            with self.subTest(name=name):
                left, right = self.pair()
                vars(left)[name] = lambda *args: (_ for _ in ()).throw(AssertionError("authored storage ran"))
                self.assertTrue(self.classify(left, right))

    def test_shadowed_instance_dictionary_is_not_evaluated(self):
        class HiddenState(self.m.Surface):
            @property
            def __dict__(self):
                raise AssertionError("authored __dict__ ran during admission")
        self.assertTrue(self.classify(HiddenState(), self.m.Surface()))

    def test_prior_authored_hook_or_material_refusal_runs_first(self):
        left, right = self.pair()
        animation = self.m.Transform(left, right)
        animation.previous_refusal = True
        self.assertTrue(self.m.Scene().play(animation)[0])
        self.assertEqual(self.m.queries, [])

    def test_mixed_plays_and_compositions_keep_ordered_callbacks(self):
        left, right = self.pair()
        animation = self.m.Transform(left, right)
        scene = self.m.Scene()
        self.assertEqual(scene.play(animation, self.m.Animation()), (True, False))
        self.assertEqual(scene.play(self.m.Composition(animation)), ((True,),))
        self.assertTrue(self.m._requires_python_animation(animation))

    def test_builder_is_built_once_without_changing_its_product(self):
        left, right = self.pair()
        animation = self.m.Transform(left, right)
        builder = self.m._AnimationBuilder(animation)
        self.assertEqual(self.m.Scene().play(builder), (False,))
        self.assertEqual(builder.builds, 1)
        self.assertTrue(self.m._requires_python_animation(animation))

    def test_invalid_builder_product_fails_before_play(self):
        builder = self.m._AnimationBuilder(object())
        with self.assertRaisesRegex(TypeError, "must return an Animation"):
            self.m.Scene().play(builder)
        self.assertEqual(builder.builds, 1)

    def test_topology_is_rechecked_after_authored_pre_play_mutation(self):
        left, right = self.pair()
        animation = self.m.Transform(left, right)
        scene = self.m.Scene()
        scene.before_admission = lambda: setattr(left, "native_shape", (4, 3))
        self.assertEqual(scene.play(animation), (True,))

    def test_scope_restored_after_failure(self):
        left, right = self.pair()
        animation = self.m.Transform(left, right)
        scene = self.m.Scene()
        scene.fail = True
        with self.assertRaisesRegex(RuntimeError, "authored failure"):
            scene.play(animation)
        self.assertTrue(self.m._requires_python_animation(animation))
        self.assertEqual(self.m.Scene().play(animation), (False,))

    def test_native_binding_replacement_is_not_trusted_or_called(self):
        left, right = self.pair()
        self.m._surface_grid_resolution = lambda member: (_ for _ in ()).throw(
            AssertionError("replacement binding was called"))
        self.assertTrue(self.classify(left, right))

    def test_old_native_builtin_identity_does_not_keep_its_owner_alive(self):
        reference = ref(self.m._surface_grid_resolution)
        self.m._surface_grid_resolution = lambda member: None
        self.assertIsNone(reference())

    def test_invalid_grid_keeps_existing_lifecycle_error_channel(self):
        left, right = self.pair()
        left.native_shape = "invalid"
        self.assertTrue(self.classify(left, right))

    def test_non_surface_transforms_are_unchanged(self):
        self.assertFalse(self.classify(self.m.Mobject(), self.m.Mobject()))
        self.assertEqual(self.m.queries, [])


if __name__ == "__main__":
    unittest.main()
