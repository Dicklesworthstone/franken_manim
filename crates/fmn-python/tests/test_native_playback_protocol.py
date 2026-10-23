"""Admission never calls authored camera/light code or trusts late patches."""
from pathlib import Path
import sys
from types import SimpleNamespace
import unittest

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from fmn_python.native_playback import install_native_playback


def fixture():
    calls = []

    class Children(list):
        pass

    class Point:
        def __init__(self):
            self.submobjects = Children()

        def get_center(self):
            calls.append("center")
            return (0, 0, 0)

        get_bounding_box = compute_bounding_box = get_family = get_center
        _bbox_rows = _is_bound = _get_bbox = _get_installed_bbox = _has_points = get_center

    class Frame:
        def __init__(self):
            self._core = object()

    class Camera:
        def __init__(self):
            self.frame = Frame()
            self.background_rgba = list(np.array([0, 0, 0, 1], dtype=float))
            self.light_source = Point()

    class Scene:
        def __init__(self):
            self._camera = Camera()

        @property
        def camera(self):
            calls.append("camera")
            return self._camera

    native = SimpleNamespace(Scene=Scene, Camera=Camera, CameraFrame=Frame,
                             Point=Point, _LiveSubmobjects=Children, _np=np)
    install_native_playback(native)
    native._fmn_finalize_native_playback()
    scene = Scene()
    return native, scene, calls


class NativePlaybackProtocolTests(unittest.TestCase):
    def test_stock_numpy_background_and_camera_require_no_getter_calls(self):
        native, scene, calls = fixture()
        self.assertIs(native._fmn_native_playback_light(scene), scene._camera.light_source)
        self.assertEqual(calls, [])

    def test_missing_lazy_camera_keeps_stepped_initialization(self):
        native, scene, calls = fixture()
        del scene._camera
        self.assertIsNone(native._fmn_native_playback_light(scene))
        self.assertEqual(calls, [])

    def test_authored_scene_camera_property_is_not_evaluated(self):
        native, scene, calls = fixture()

        class Authored(native.Scene):
            @property
            def camera(self):
                calls.append("authored camera")
                raise AssertionError("admission ran a camera getter")

        scene.__class__ = Authored
        self.assertIsNone(native._fmn_native_playback_light(scene))
        self.assertEqual(calls, [])

    def test_authored_attribute_lookup_is_not_evaluated(self):
        native, scene, calls = fixture()

        class Authored(native.Scene):
            def __getattribute__(self, name):
                calls.append(name)
                return super().__getattribute__(name)

        scene.__class__ = Authored
        self.assertIsNone(native._fmn_native_playback_light(scene))
        self.assertEqual(calls, [])

    def test_shadowed_instance_dictionary_is_not_evaluated(self):
        native, scene, calls = fixture()

        class Authored(native.Scene):
            @property
            def __dict__(self):
                calls.append("dict")
                raise AssertionError("admission ran a dictionary getter")

        scene.__class__ = Authored
        self.assertIsNone(native._fmn_native_playback_light(scene))
        self.assertEqual(calls, [])

    def test_instance_light_center_and_nested_bbox_overrides_demote(self):
        for name in ("get_center", "compute_bounding_box", "get_family", "_get_bbox"):
            with self.subTest(name=name):
                native, scene, calls = fixture()
                setattr(scene._camera.light_source, name, lambda: calls.append(name))
                self.assertIsNone(native._fmn_native_playback_light(scene))
                self.assertEqual(calls, [])

    def test_class_patches_cannot_become_the_first_play_baseline(self):
        native, scene, calls = fixture()
        native.Point.get_center = lambda self: calls.append("patched center")
        self.assertIsNone(native._fmn_native_playback_light(scene))
        self.assertEqual(calls, [])
        # Idempotent package initialization must not bless a late patch.
        install_native_playback(native)
        self.assertIsNone(native._fmn_native_playback_light(scene))

    def test_camera_and_frame_descriptors_are_not_evaluated(self):
        for cls_name, attr in (("Camera", "frame"), ("Camera", "background_rgba"),
                               ("Camera", "light_source"), ("CameraFrame", "_core")):
            with self.subTest(cls=cls_name, attr=attr):
                native, scene, calls = fixture()
                setattr(getattr(native, cls_name), attr,
                        property(lambda self: calls.append("descriptor")))
                self.assertIsNone(native._fmn_native_playback_light(scene))
                self.assertEqual(calls, [])

    def test_custom_background_conversion_is_not_evaluated(self):
        native, scene, calls = fixture()

        class Value:
            def __float__(self):
                calls.append("float")
                return 1.0

        scene._camera.background_rgba[-1] = Value()
        self.assertIsNone(native._fmn_native_playback_light(scene))
        self.assertEqual(calls, [])

    def test_composite_light_retains_family_observers(self):
        native, scene, calls = fixture()
        scene._camera.light_source.submobjects.append(native.Point())
        self.assertIsNone(native._fmn_native_playback_light(scene))
        self.assertEqual(calls, [])

    def test_light_container_iterator_override_is_not_called(self):
        native, scene, calls = fixture()
        native._LiveSubmobjects.__iter__ = lambda self: calls.append("iter")
        self.assertIsNone(native._fmn_native_playback_light(scene))
        self.assertEqual(calls, [])

    def test_empty_plain_updater_storage_is_admitted_without_iteration(self):
        native, scene, calls = fixture()
        member = native.Point()
        member.updaters = []
        self.assertIs(native._fmn_native_playback_light(scene, [member]),
                      scene._camera.light_source)
        self.assertEqual(calls, [])

    def test_empty_custom_updater_iterator_demotes_without_calling_it(self):
        native, scene, calls = fixture()

        class Updaters(list):
            def __iter__(self):
                calls.append("updater iterator")
                return super().__iter__()

        member = native.Point()
        member.updaters = Updaters()
        self.assertIsNone(native._fmn_native_playback_light(scene, [member]))
        self.assertEqual(calls, [])

    def test_updater_descriptor_demotes_without_calling_it(self):
        native, scene, calls = fixture()

        class Member:
            @property
            def updaters(self):
                calls.append("updater property")
                return []

        self.assertIsNone(native._fmn_native_playback_light(scene, [Member()]))
        self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main()
