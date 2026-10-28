"""Native SVG subclass construction, factory identity and rendered witnesses."""
from __future__ import annotations

import copy
import importlib
import inspect
from pathlib import Path
import pickle
import tempfile
import unittest
from unittest.mock import patch

import numpy as np
import manimlib as m

SOURCE = '<svg><path d="M0 0H2V1H0Z" fill="red"/></svg>'
DASHED = '<svg><path d="M0 0H2V2H0Z" fill="blue" stroke="red" stroke-width="6" stroke-dasharray=".5 .5"/></svg>'


def pixels(obj):
    return m.Camera(resolution=(96, 54)).capture_snapshot(obj).pixels()


class SvgLifecycleTests(unittest.TestCase):
    def test_small_and_large_svg_viewboxes_keep_the_cubic_bound_after_sizing(self):
        for size in (16.0, 1000.0):
            source = (
                f'<svg viewBox="0 0 {size} {size}">'
                f'<path d="M0 0C{size} 0 0 {size} {size} {size}"/></svg>'
            )
            for width in (None, 9.0):
                with self.subTest(viewbox=size, width=width):
                    obj = m.SVGMobject(svg_string=source, height=4, width=width)
                    final_size = 4.0 if width is None else width
                    self.assertAlmostEqual(obj.get_height(), final_size, places=5)
                    self.assertAlmostEqual(obj.get_width(), final_size, places=5)
                    self.assertNotIn("_fmn_svg_import_size", vars(obj))
                    points = obj[0].get_points()
                    start = np.array([-final_size / 2, final_size / 2, 0])
                    if np.linalg.norm(points[0] - start) > 1e-5:
                        points = points[::-1]
                    self.assertLess(np.linalg.norm(points[0] - start), 1e-5)
                    count = (len(points) - 1) // 2
                    t = np.linspace(0, 1, 4097)
                    index = np.minimum((t * count).astype(int), count - 1)
                    local = (t * count - index)[:, None]
                    actual = (
                        (1 - local)**2 * points[2 * index]
                        + 2 * local * (1 - local) * points[2 * index + 1]
                        + local**2 * points[2 * index + 2]
                    )
                    expected = np.column_stack((
                        final_size * (3*t - 6*t*t + 4*t*t*t) - final_size/2,
                        final_size/2 - final_size * (3*t*t - 2*t*t*t),
                        np.zeros_like(t),
                    ))
                    # Native records are f32; leave only their rounding margin
                    # beyond the canonical 0.1-pixel scene-space bound.
                    self.assertLessEqual(
                        np.linalg.norm(actual - expected, axis=1).max(),
                        0.1 / 135 + 2e-6,
                    )

    def test_svg_pending_size_is_cleared_when_an_authored_hook_fails(self):
        seen = []
        class Broken(m.SVGMobject):
            def init_svg_mobject(self):
                seen.append(self)
                raise ValueError("authored SVG construction failed")
        with self.assertRaisesRegex(ValueError, "authored SVG construction failed"):
            Broken(svg_string=SOURCE, height=4)
        self.assertEqual(len(seen), 1)
        self.assertNotIn("_fmn_svg_import_size", vars(seen[0]))

    def test_svg_refuses_curve_loss_at_both_placement_bakes(self):
        cases = (
            # Exact initial records still lose precision after final sizing.
            ("M0 0Q1 1 3 7", 1_000_000),
            # Initial and ideal final records are exact. The mandatory
            # flip/orientation bake rounds the intermediate handle instead.
            ("M0 0Q1 1 2 33554432", 65_536),
        )
        for path, height in cases:
            with self.subTest(path=path, height=height):
                source = f'<svg><path d="{path}"/></svg>'
                ordinary = m.SVGMobject(svg_string=source, height=4)
                self.assertAlmostEqual(ordinary.get_height(), 4, places=5)
                with self.assertRaisesRegex(ValueError, "f32 records.*curve tolerance"):
                    m.SVGMobject(svg_string=source, height=height)

    def test_svg_refuses_new_break_markers_during_flip_and_final_bake(self):
        # Class defaults are the Reference's source of an explicitly unsized
        # constructor; passing None alone means to use those defaults.
        class Unsized(m.SVGMobject):
            height = None
            width = None
        cases = (
            (Unsized, "M0 0Q0 0.015625 2 33554432", {}),
            (m.SVGMobject, "M0 0Q0.00000001 0 1 1", {"height": 4}),
        )
        for factory, path, options in cases:
            with self.subTest(path=path):
                source = f'<svg><path d="{path}"/></svg>'
                with self.assertRaisesRegex(ValueError, "shared-anchor path topology"):
                    factory(svg_string=source, **options)

    def test_svg_refuses_large_offsets_and_sequential_scale_clamping(self):
        source = '<svg><path d="M1000000 0Q1000005.1 6.2 1000016 16"/></svg>'
        with self.assertRaisesRegex(ValueError, "f32 records.*curve tolerance"):
            m.SVGMobject(svg_string=source, height=4)
        with self.assertRaisesRegex(ValueError, "native minimum"):
            m.SVGMobject(svg_string=SOURCE, height=1e12, width=4)

    def test_unsized_centering_checks_topology_but_raw_factory_parts_stay_unframed(self):
        class Unsized(m.SVGMobject):
            height = None
            width = None
        source = ('<svg><path d="M0 0Q0.125 0 2 0"/>'
                  '<path d="M33554432 0"/></svg>')
        with self.assertRaisesRegex(ValueError, "shared-anchor path topology"):
            Unsized(svg_string=source)
        uncentered = Unsized(svg_string=source, should_center=False)
        np.testing.assert_array_equal(uncentered[0].get_points()[:, 0], [0, 0.125, 2])
        # This public factory is also callable after construction. It returns
        # raw viewport geometry, with no promised flip or centering phase.
        receiver = m.SVGMobject(svg_string=SOURCE)
        parts = receiver.mobjects_from_svg_string(source)
        self.assertEqual(len(parts), 2)
        np.testing.assert_array_equal(parts[0].get_points()[:, 0], [0, 0.125, 2])
        raw_flip_witness = '<svg><path d="M0 0Q0 0.015625 2 33554432"/></svg>'
        parts = receiver.mobjects_from_svg_string(raw_flip_witness)
        np.testing.assert_array_equal(parts[0].get_points()[:, 1], [0, 0.015625, 33554432])

    def test_svg_authored_centering_truthiness_runs_once_after_assembly(self):
        events = []
        class Centering:
            def __bool__(self):
                events.append("centering")
                return True
        class Custom(m.SVGMobject):
            def init_svg_mobject(self):
                events.append("assembly")
                super().init_svg_mobject()
        obj = Custom(svg_string=SOURCE, height=4, should_center=Centering())
        self.assertEqual(events, ["assembly", "centering"])
        self.assertNotIn("_fmn_svg_import_size", vars(obj))
        np.testing.assert_allclose(obj.get_center(), 0, atol=1e-6)

    def test_hook_order_and_source_recipe(self):
        events = []
        class Custom(m.SVGMobject):
            def init_data(self):
                events.append(('data', self.svg_string))
                super().init_data()
            def init_points(self):
                events.append(('points', self.get_num_points()))
                super().init_points()
            def init_uniforms(self):
                events.append(('uniforms',))
                super().init_uniforms()
            def init_colors(self):
                events.append(('colors', len(self)))
                super().init_colors()
            def init_svg_mobject(self):
                events.append(('svg',))
                super().init_svg_mobject()
            def mobjects_from_svg_string(self, source):
                events.append(('parts', source))
                return super().mobjects_from_svg_string(source)
        obj = Custom(svg_string=SOURCE)
        self.assertEqual(events, [('data', SOURCE), ('points', 0), ('uniforms',),
                                  ('colors', 0), ('svg',), ('parts', SOURCE)])
        self.assertEqual(len(obj), 1)

    def test_custom_columns_root_geometry_and_decorations_survive(self):
        class Custom(m.SVGMobject):
            data_dtype = m.VMobject.data_dtype + [('weight', 1)]
            def init_data(self):
                super().init_data()
                self.decoration = m.Dot(m.RIGHT)
                self.add(self.decoration)
            def init_points(self):
                self.set_points_as_corners([[-1, 0, 0], [0, 1, 0], [1, 0, 0]])
                self.data['weight'][:] = 7
                self.weights = self.data['weight']
            def init_uniforms(self):
                super().init_uniforms()
                self.uniforms['fixed_in_frame'] = .75
        obj = Custom(svg_string=SOURCE, height=2)
        self.assertEqual(obj.get_num_points(), 5)
        self.assertIs(obj[0], obj.decoration)
        np.testing.assert_array_equal(obj.data['weight'], 7)
        self.assertEqual(obj.uniforms['fixed_in_frame'], .75)
        obj.weights[:] = 9
        np.testing.assert_array_equal(obj.data['weight'], 9)
        scene = m.Scene(); scene.add(obj)
        scene.play(obj.animate.shift(m.RIGHT), run_time=1/30, rate_func=m.linear)
        np.testing.assert_array_equal(obj.data['weight'], 9)

    def test_returned_parts_are_actual_children_not_reconstructed(self):
        made, calls = [], []
        class Custom(m.SVGMobject):
            def mobjects_from_svg_string(self, source):
                calls.append(source)
                for point in (m.LEFT, m.RIGHT):
                    obj = m.Square(side_length=.5).shift(point).set_fill(m.GREEN, 1)
                    made.append(obj)
                    yield obj
        obj = Custom(svg_string='authored source', height=1)
        self.assertEqual(calls, ['authored source'])
        self.assertEqual(list(obj.submobjects), made)
        self.assertTrue(all(child.get_fill_color() == m.GREEN for child in made))

    def test_init_svg_override_can_supply_the_complete_family(self):
        class Custom(m.SVGMobject):
            def init_svg_mobject(self):
                self.authored = m.Triangle().set_fill(m.YELLOW, 1)
                self.add(self.authored)
        obj = Custom(svg_string='no XML parsing requested by this subclass', height=2)
        self.assertEqual(list(obj), [obj.authored])
        self.assertEqual(obj.authored.get_fill_color(), m.YELLOW)

    def test_factory_edits_native_paint_layers_without_filling_dashes(self):
        class Custom(m.SVGMobject):
            def mobjects_from_svg_string(self, source):
                parts = super().mobjects_from_svg_string(source)
                for part in parts:
                    part.stretch(1.5, 0)
                return parts
        obj = Custom(svg_string=DASHED, height=2, stroke_width=None)
        expected = m.SVGMobject(svg_string=DASHED, height=2, stroke_width=None)
        expected.stretch(1.5, 0)
        self.assertEqual(pixels(obj), pixels(expected))
        obj.set_opacity(.4)
        self.assertTrue(all(layer.get_fill_opacity() == 0 for layer in obj[0].submobjects[1:]))

    def test_live_rebuild_invokes_public_factory_once_and_appends(self):
        obj = m.SVGMobject(svg_string=SOURCE)
        scene = m.Scene(); scene.add(obj)
        old = list(obj.submobjects)
        made = m.Circle(radius=.25)
        calls = []
        obj.mobjects_from_svg_string = lambda source: calls.append(source) or [made]
        self.assertIsNone(obj.init_svg_mobject())
        self.assertEqual(calls, [SOURCE])
        self.assertEqual(list(obj.submobjects), [*old, made])
        self.assertIs(made._scene, scene)
        self.assertEqual(scene.get_time(), 0)

    def test_generator_failure_does_not_install_half_a_family(self):
        obj = m.SVGMobject(svg_string=SOURCE)
        before = list(obj.submobjects); frame = pixels(obj)
        failure = RuntimeError('authored SVG factory')
        def factory(source):
            yield m.Square()
            raise failure
        obj.mobjects_from_svg_string = factory
        with self.assertRaises(RuntimeError) as caught:
            obj.init_svg_mobject()
        self.assertIs(caught.exception, failure)
        self.assertEqual(list(obj.submobjects), before)
        self.assertEqual(pixels(obj), frame)

    def test_foreign_reused_and_invalid_parts_are_refused_without_mutation(self):
        obj = m.SVGMobject(svg_string=SOURCE)
        foreign = m.Square(); scene = m.Scene(); scene.add(foreign)
        before = foreign.get_points().copy(); children = list(obj.submobjects)
        repeated = m.Square()
        bad = m.VMobject().set_points_as_corners([[0, 0, 0], [1, 0, 0]])
        bad.data['point'][0, 0] = float('nan')
        for result in ([foreign], [obj], [obj[0]], [repeated, repeated], [None], [bad]):
            with self.subTest(result=result):
                obj.mobjects_from_svg_string = lambda source: iter(result)
                with self.assertRaises((TypeError, ValueError)):
                    obj.init_svg_mobject()
                self.assertEqual(list(obj.submobjects), children)
                np.testing.assert_array_equal(foreign.get_points(), before)

    def test_shared_descendants_keep_identity(self):
        shared = m.Dot()
        left, right = m.VGroup(shared), m.VGroup(shared)
        class Custom(m.SVGMobject):
            def mobjects_from_svg_string(self, source):
                return [left, right]
        obj = Custom(svg_string=SOURCE)
        self.assertIs(obj[0][0], obj[1][0])
        clone = obj.copy()
        self.assertIs(clone[0][0], clone[1][0])
        self.assertIsNot(clone[0][0], shared)

    def test_later_getter_cannot_adopt_an_earlier_part_unnoticed(self):
        obj = m.SVGMobject(svg_string=SOURCE); previous = list(obj.submobjects)
        first, second, scene = m.Square(), m.Circle(), m.Scene()
        original = second.get_points
        def points():
            scene.add(first)
            return original()
        second.get_points = points
        obj.mobjects_from_svg_string = lambda source: [first, second]
        with self.assertRaisesRegex(ValueError, 'ownership changed'):
            obj.init_svg_mobject()
        self.assertEqual(list(obj.submobjects), previous)
        self.assertIs(first._scene, scene)

    def test_failed_hook_stops_later_phases_without_retry(self):
        events = []; failure = ValueError('authored points')
        class Custom(m.SVGMobject):
            def init_points(self):
                events.append('points')
                raise failure
            def init_uniforms(self):
                events.append('uniforms')
            def init_svg_mobject(self):
                events.append('svg')
        with self.assertRaises(ValueError) as caught:
            Custom(svg_string=SOURCE)
        self.assertIs(caught.exception, failure)
        self.assertEqual(events, ['points'])

    def test_reentrant_factory_unwinds_and_allows_next_rebuild(self):
        obj = m.SVGMobject(svg_string=SOURCE)
        previous = list(obj.submobjects)
        default = obj.mobjects_from_svg_string
        obj.mobjects_from_svg_string = lambda source: obj.init_svg_mobject()
        with self.assertRaisesRegex(RuntimeError, 'reenter'):
            obj.init_svg_mobject()
        self.assertEqual(list(obj.submobjects), previous)
        obj.mobjects_from_svg_string = default
        obj.init_svg_mobject()
        self.assertEqual(len(obj), len(previous) * 2)

    def test_bounds_reject_before_adoption(self):
        import fmn_python.svg_ingress as ingress
        obj = m.SVGMobject(svg_string=SOURCE); previous = list(obj.submobjects)
        obj.mobjects_from_svg_string = lambda source: (m.Square() for _ in range(4))
        with patch.object(ingress, '_MAX_PARTS', 3), self.assertRaisesRegex(ValueError, 'budget'):
            obj.init_svg_mobject()
        self.assertEqual(list(obj.submobjects), previous)

    def test_copy_and_pickle_keep_svg_family_independent(self):
        source = m.SVGMobject(svg_string=DASHED, stroke_width=None)
        before = pixels(source)
        for clone in (source.copy(), copy.deepcopy(source), pickle.loads(pickle.dumps(source))):
            clone.svg_string = SOURCE
            old = list(clone.submobjects)
            clone.init_svg_mobject()
            self.assertEqual(len(clone), len(old) + 1)
            self.assertIsNot(clone[0], source[0])
            self.assertEqual(pixels(source), before)

    def test_frames_match_literal_geometry_at_one_and_four_threads(self):
        class Custom(m.SVGMobject):
            def mobjects_from_svg_string(self, source):
                return [part.stretch(1.5, 0) for part in super().mobjects_from_svg_string(source)]
        def render(path, factory, threads):
            scene = m.Scene()
            with scene.render_session(path, format='png_sequence', resolution=(96, 54), fps=8, threads=threads):
                obj = factory(); scene.add(obj)
                scene.play(obj.animate.shift(m.RIGHT), run_time=.5, rate_func=m.linear)
            return [file.read_bytes() for file in sorted(path.glob('*.png'))]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            authored = lambda: Custom(svg_string=SOURCE, height=1)
            literal = lambda: m.Rectangle(width=3, height=1).set_fill('#ff0000', 1).set_stroke(width=0)
            frames = render(root/'one', authored, 1)
            self.assertEqual(frames, render(root/'four', authored, 4))
            self.assertEqual(frames, render(root/'literal', literal, 1))
            self.assertEqual(len(frames), 4)
            self.assertGreater(len(set(frames)), 1)

    def test_aliases_signatures_and_missing_source(self):
        exported = importlib.import_module('manimlib.mobject.svg.svg_mobject')
        self.assertIs(exported.SVGMobject, m.SVGMobject)
        self.assertIn('svg_default', inspect.signature(m.SVGMobject).parameters)
        with self.assertRaisesRegex(Exception, 'Must specify either'):
            m.SVGMobject()


if __name__ == '__main__':
    unittest.main(verbosity=2)
