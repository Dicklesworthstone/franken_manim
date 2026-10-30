"""fm-fc53: positional operations on point-less mobjects, against the Reference.

Every expected value below was measured in the pinned Reference
(3b1b/manim@6199a00d) by running the same construction there; the
Reference returns identical values whether or not the mobject is in a Scene.
Each case runs here twice: on detached mobjects (the portal's per-proxy
nursery stages and Python family graph) and on scene-bound ones (one Stage).

Scope: shift/move_to/next_to/align_to/to_edge/to_corner, scale/stretch/
rotate about points, the getters, families, copies and the box refreshes
(point edits, family edits, non-box-preserving maps). Interpolating a
point-less box is ruled by Appendix C-33 rather than measured, because the
Reference's result there depends on an earlier read (PointFreeInterpolation
below).
"""
import copy
import unittest

import numpy as np
import manimlib as m

REFERENCE = {
    'add_empty_after_shift': {"child": [0.0, 0.0, 0.0], "root": [0.0, 0.0, 0.0]},
    'add_points_child_to_shifted_empty_then_remove': [0.0, 0.0, 0.0],
    'add_square_after_shift': [0.0, 0.0, 0.0],
    'align_to_mob_up': [0.0, 3.0, 0.0],
    'align_to_point_left': [-3.0, 3.0, 4.0],
    'apply_function': [0.0, 0.0, 0.0],
    'apply_matrix': [0.0, 0.0, 0.0],
    'apply_points_function_bbox': [3.0, 0.0, 0.0],
    'apply_points_function_plain': [0.0, 0.0, 0.0],
    'arrange_all_empty': [[0.0, 0.0, 0.0], [0.0, -0.25, 0.0], [0.0, -0.5, 0.0], [0.0, 0.0, 0.0]],
    'arrange_in_grid_empties': [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [1.0, -1.0, 0.0]],
    'arrange_with_empty_member': [[-1.25, 0.0, 0.0], [0.0, 0.0, 0.0], [1.25, 0.0, 0.0], [0.0, 0.0, 0.0]],
    'become_empty': [2.0, 3.0, 4.0],
    'center_after_shift': [0.0, 0.0, 0.0],
    'child_shift_refreshes_empty_parent': {"inner": [2.0, 0.0, 0.0], "outer": [0.0, 0.0, 0.0]},
    'classes_shift': {"Group": [2.0, 3.0, 4.0], "Mobject": [2.0, 3.0, 4.0],
                      "VGroup": [2.0, 3.0, 4.0], "VMobject": [2.0, 3.0, 4.0]},
    'copy_family_with_empty_child': [5.0, 0.0, 0.0],
    'copy_keeps_position': [[2.0, 3.0, 4.0], [2.0, 3.0, 4.0], [2.0, 3.0, 4.0]],
    'empty_child_only': {"inner": [5.0, 0.0, 0.0], "outer": [0.0, 0.0, 0.0]},
    'empty_child_with_square': {"inner": [5.0, 0.0, 0.0],
                                "outer": [[-1.0, -1.0, 0.0], [0.0, 0.0, 0.0], [1.0, 1.0, 0.0]]},
    'explicit_refresh': [0.0, 0.0, 0.0],
    'flip': [0.0, 0.0, 0.0],
    'flip_family_with_empty_child_and_points': [0.0, 0.0, 0.0],
    'fresh_getters': {"bbox": [[0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]],
                      "center": [0.0, 0.0, 0.0], "width": 0.0},
    'generate_target': [[2.0, 2.0, 4.0], [2.0, 3.0, 4.0]],
    'getters': {"all_corners": [[2.0, 3.0, 4.0]] * 8,
                "bbox": [[2.0, 3.0, 4.0], [2.0, 3.0, 4.0], [2.0, 3.0, 4.0]],
                "bbox_point_ul": [2.0, 3.0, 4.0], "bottom": [2.0, 3.0, 4.0],
                "center": [2.0, 3.0, 4.0], "coord_top": 3.0, "corner_ur": [2.0, 3.0, 4.0],
                "depth": 0.0, "edge_center_down": [2.0, 3.0, 4.0], "height": 0.0,
                "left": [2.0, 3.0, 4.0], "nadir": [2.0, 3.0, 4.0], "right": [2.0, 3.0, 4.0],
                "shape": [0.0, 0.0, 0.0], "top": [2.0, 3.0, 4.0], "width": 0.0,
                "x": 2.0, "y": 3.0, "z": 4.0, "zenith": [2.0, 3.0, 4.0]},
    'group_of_shifted_empty': {"inner": [2.0, 0.0, 0.0], "outer": [0.0, 0.0, 0.0]},
    'is_touching_point': True,
    'match_points_empty_source': [0.0, 0.0, 0.0],
    'match_x': [-5.0, 3.0, 4.0],
    'move_to': [0.0, 3.0, 0.0],
    'move_to_mob_aligned_edge': [-2.0, 1.0, 0.0],
    'nested_empty_shift': {"inner": [1.0, 0.0, 0.0], "outer": [1.0, 0.0, 0.0]},
    'next_to_point_aligned': [1.5, 1.5, 0.0],
    'next_to_square_down': [0.0, -1.25, 0.0],
    'remove_last_points': [0.0, 0.0, 0.0],
    'rotate_about_center': [[0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]],
    'rotate_about_origin': [[0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]],
    'rotate_about_point': [0.0, 0.0, 0.0],
    'rotate_family_with_empty_child': {"inner": [0.0, 0.0, 0.0], "root": [0.0, 0.0, 0.0]},
    'save_state_restore': [2.0, 3.0, 4.0],
    'scale_about_center': [[2.0, 3.0, 4.0], [2.0, 3.0, 4.0], [2.0, 3.0, 4.0]],
    'scale_about_edge': [[2.0, 3.0, 4.0], [2.0, 3.0, 4.0], [2.0, 3.0, 4.0]],
    'scale_about_origin': [[4.0, 6.0, 8.0], [4.0, 6.0, 8.0], [4.0, 6.0, 8.0]],
    'scale_family_with_empty_child': {"inner": [2.0, 0.0, 0.0],
                                      "root": [[-2.0, -2.0, 0.0], [0.0, 0.0, 0.0], [2.0, 2.0, 0.0]]},
    'scale_vector_about_origin': [[4.0, 9.0, 4.0], [4.0, 9.0, 4.0], [4.0, 9.0, 4.0]],
    'set_color_keeps_position': [2.0, 3.0, 4.0],
    'set_points_after_shift': [1.0, 2.0, 3.0],
    'set_submobjects_empty': [1.0, 0.0, 0.0],
    'set_width_empty': [[1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 0.0]],
    'set_x_y_z': [-1.0, -2.0, -3.0],
    'shift': [2.0, 0.0, 0.0],
    'shift_parent_with_shifted_empty_child_and_square': {"inner": [5.0, 1.0, 0.0],
                                                         "outer": [0.0, 1.0, 0.0]},
    'shift_then_clear_points': [0.0, 0.0, 0.0],
    'square_next_to_empty': [3.25, 0.0, 0.0],
    'stretch_about_origin': [[4.0, 3.0, 4.0], [4.0, 3.0, 4.0], [4.0, 3.0, 4.0]],
    'to_corner_ur': [6.611111, 3.5, 0.0],
    'to_edge_left_buff': [-6.111111, 3.0, 4.0],
    'to_edge_up': [0.0, 3.5, 0.0],
}

# The class sweep's BarChart#1 (values [1, 2, 3]): its empty bar_labels group
# (path 0.4) has this box centre in the Reference, in 1e-3 units.
BAR_LABELS_CENTER = [-2.593, -5.75, 0.0]


def r(value):
    return np.asarray(value, dtype=float).tolist()


def cases(bind):
    """The constructions, keyed like REFERENCE. `bind` adds a root to a Scene
    (or does nothing) before any positional operation runs."""
    def shifted(cls=m.VGroup, v=(2.0, 3.0, 4.0)):
        g = bind(cls())
        g.shift(np.array(v))
        return g

    def square_shifted(v):
        return m.Square().shift(np.array(v, dtype=float))

    def fresh():
        return bind(m.VGroup())

    out = {}

    def case(fn):
        out[fn.__name__] = fn
        return fn

    @case
    def shift():
        g = fresh(); g.shift(2 * m.RIGHT); return r(g.get_center())

    @case
    def move_to():
        g = fresh(); g.shift(m.RIGHT); g.move_to(3 * m.UP); return r(g.get_center())

    @case
    def move_to_mob_aligned_edge():
        g = fresh(); g.move_to(square_shifted(m.LEFT), aligned_edge=m.UL); return r(g.get_center())

    @case
    def next_to_square_down():
        g = fresh(); g.next_to(m.Square(), m.DOWN); return r(g.get_center())

    @case
    def next_to_point_aligned():
        g = fresh()
        g.next_to(np.array([1.0, 1.0, 0.0]), m.UR, buff=0.5, aligned_edge=m.LEFT)
        return r(g.get_center())

    @case
    def square_next_to_empty():
        s = m.Square(); s.next_to(shifted(v=(2, 0, 0)), m.RIGHT); return r(s.get_center())

    @case
    def align_to_mob_up():
        g = fresh(); g.align_to(square_shifted(2 * m.UP), m.UP); return r(g.get_center())

    @case
    def align_to_point_left():
        g = shifted(); g.align_to(np.array([-3.0, 7.0, 0.0]), m.LEFT); return r(g.get_center())

    @case
    def to_edge_up():
        g = fresh(); g.to_edge(m.UP); return r(g.get_center())

    @case
    def to_edge_left_buff():
        g = shifted(); g.to_edge(m.LEFT, buff=1.0); return r(g.get_center())

    @case
    def to_corner_ur():
        g = fresh(); g.to_corner(m.UR); return r(g.get_center())

    @case
    def center_after_shift():
        g = shifted(); g.center(); return r(g.get_center())

    @case
    def set_x_y_z():
        g = shifted(); g.set_x(-1); g.set_y(-2); g.set_z(-3); return r(g.get_center())

    @case
    def match_x():
        g = shifted(); g.match_x(square_shifted(5 * m.LEFT)); return r(g.get_center())

    @case
    def scale_about_center():
        g = shifted(); g.scale(7); return r(g.get_bounding_box())

    @case
    def scale_about_origin():
        g = shifted(); g.scale(2, about_point=m.ORIGIN); return r(g.get_bounding_box())

    @case
    def scale_about_edge():
        g = shifted(); g.scale(3, about_edge=m.UL); return r(g.get_bounding_box())

    @case
    def scale_vector_about_origin():
        g = shifted(); g.scale(np.array([2.0, 3.0, 1.0]), about_point=m.ORIGIN)
        return r(g.get_bounding_box())

    @case
    def stretch_about_origin():
        g = shifted(); g.stretch(2, 0, about_point=m.ORIGIN); return r(g.get_bounding_box())

    @case
    def rotate_about_origin():
        g = shifted(); g.rotate(m.PI / 2, about_point=m.ORIGIN); return r(g.get_bounding_box())

    @case
    def rotate_about_center():
        g = shifted(); g.rotate(m.PI / 2); return r(g.get_bounding_box())

    @case
    def rotate_about_point():
        g = shifted(); g.rotate(m.PI / 2, about_point=np.array([1.0, 1.0, 0.0]))
        return r(g.get_center())

    @case
    def flip():
        g = shifted(); g.flip(); return r(g.get_center())

    @case
    def apply_matrix():
        g = shifted(); g.apply_matrix([[2, 0], [0, 2]]); return r(g.get_center())

    @case
    def apply_function():
        g = shifted(); g.apply_function(lambda p: 2 * p); return r(g.get_center())

    @case
    def apply_points_function_bbox():
        g = fresh(); g.shift(m.RIGHT)
        g.apply_points_function(lambda p: p * np.array([2.0, 1.0, 1.0]) + np.array([1.0, 0.0, 0.0]),
                                about_point=m.ORIGIN, works_on_bounding_box=True)
        return r(g.get_center())

    @case
    def apply_points_function_plain():
        g = fresh(); g.shift(m.RIGHT)
        g.apply_points_function(lambda p: p + 1.0, about_point=m.ORIGIN)
        return r(g.get_center())

    @case
    def getters():
        g = shifted()
        return {
            "bbox": r(g.get_bounding_box()), "center": r(g.get_center()),
            "top": r(g.get_top()), "bottom": r(g.get_bottom()),
            "left": r(g.get_left()), "right": r(g.get_right()),
            "zenith": r(g.get_zenith()), "nadir": r(g.get_nadir()),
            "corner_ur": r(g.get_corner(m.UR)),
            "edge_center_down": r(g.get_edge_center(m.DOWN)),
            "bbox_point_ul": r(g.get_bounding_box_point(m.UL)),
            "all_corners": r(g.get_all_corners()),
            "width": r(g.get_width()), "height": r(g.get_height()), "depth": r(g.get_depth()),
            "x": r(g.get_x()), "y": r(g.get_y()), "z": r(g.get_z()),
            "coord_top": r(g.get_coord(1, m.UP)), "shape": r(g.get_shape()),
        }

    @case
    def fresh_getters():
        g = fresh()
        return {"bbox": r(g.get_bounding_box()), "center": r(g.get_center()), "width": r(g.get_width())}

    @case
    def classes_shift():
        return {cls.__name__: r(shifted(cls).get_center())
                for cls in (m.Mobject, m.Group, m.VMobject, m.VGroup)}

    @case
    def is_touching_point():
        return bool(shifted(v=(2, 0, 0)).is_point_touching(np.array([2.0, 0.0, 0.0])))

    @case
    def set_width_empty():
        g = fresh(); g.shift(m.RIGHT); g.set_width(3); return r(g.get_bounding_box())

    @case
    def set_color_keeps_position():
        g = shifted(); g.set_color(m.RED); g.set_opacity(0.5); return r(g.get_center())

    # Families: a member's box is its own while its family has no points.
    @case
    def nested_empty_shift():
        inner = m.VGroup(); outer = bind(m.VGroup(inner)); outer.shift(m.RIGHT)
        return {"outer": r(outer.get_center()), "inner": r(inner.get_center())}

    @case
    def child_shift_refreshes_empty_parent():
        inner = m.VGroup(); outer = bind(m.VGroup(inner)); outer.shift(m.RIGHT); inner.shift(m.RIGHT)
        return {"outer": r(outer.get_center()), "inner": r(inner.get_center())}

    @case
    def empty_child_only():
        inner = m.VGroup().shift(5 * m.RIGHT); outer = bind(m.VGroup(inner))
        return {"outer": r(outer.get_center()), "inner": r(inner.get_center())}

    @case
    def group_of_shifted_empty():
        inner = m.VGroup().shift(2 * m.RIGHT); outer = bind(m.Group(inner))
        return {"outer": r(outer.get_center()), "inner": r(inner.get_center())}

    @case
    def empty_child_with_square():
        inner = m.VGroup().shift(5 * m.RIGHT); outer = bind(m.VGroup(inner, m.Square()))
        return {"outer": r(outer.get_bounding_box()), "inner": r(inner.get_center())}

    @case
    def shift_parent_with_shifted_empty_child_and_square():
        inner = m.VGroup().shift(5 * m.RIGHT); outer = bind(m.VGroup(inner, m.Square()))
        outer.shift(m.UP)
        return {"outer": r(outer.get_center()), "inner": r(inner.get_center())}

    @case
    def scale_family_with_empty_child():
        inner = m.VGroup().shift(m.RIGHT); g = bind(m.VGroup(inner, m.Square())); g.scale(2)
        return {"inner": r(inner.get_center()), "root": r(g.get_bounding_box())}

    @case
    def rotate_family_with_empty_child():
        inner = m.VGroup().shift(m.RIGHT); g = bind(m.VGroup(inner, m.Square())); g.rotate(m.PI / 2)
        return {"inner": r(inner.get_center()), "root": r(g.get_center())}

    @case
    def flip_family_with_empty_child_and_points():
        inner = m.VGroup().shift(m.RIGHT)
        g = bind(m.VGroup(inner, m.Square().shift(m.LEFT))); g.flip(m.RIGHT)
        return r(inner.get_center())

    @case
    def arrange_with_empty_member():
        g = bind(m.VGroup(m.Square(), m.VGroup(), m.Circle())); g.arrange(m.RIGHT)
        return [r(member.get_center()) for member in g] + [r(g.get_center())]

    @case
    def arrange_all_empty():
        g = bind(m.VGroup(m.VGroup(), m.VGroup(), m.VGroup())); g.arrange(m.DOWN)
        return [r(member.get_center()) for member in g] + [r(g.get_center())]

    @case
    def arrange_in_grid_empties():
        g = bind(m.VGroup(*[m.VGroup() for _ in range(4)])); g.arrange_in_grid(2, 2, buff=1)
        return [r(member.get_center()) for member in g]

    # Family edits and point edits refresh the box.
    @case
    def add_square_after_shift():
        g = fresh(); g.shift(m.RIGHT); g.add(m.Square()); return r(g.get_center())

    @case
    def add_empty_after_shift():
        g = fresh(); g.shift(m.RIGHT); g.add(m.VGroup())
        return {"root": r(g.get_center()), "child": r(g[0].get_center())}

    @case
    def remove_last_points():
        s = m.Square(); g = bind(m.VGroup(s)); g.shift(m.RIGHT); g.remove(s); return r(g.get_center())

    @case
    def add_points_child_to_shifted_empty_then_remove():
        g = fresh(); g.shift(m.RIGHT); s = m.Square(); g.add(s); g.remove(s); return r(g.get_center())

    @case
    def set_submobjects_empty():
        # An unchanged child list returns early, with no refresh.
        g = fresh(); g.shift(m.RIGHT); g.set_submobjects([]); return r(g.get_center())

    @case
    def explicit_refresh():
        g = fresh(); g.shift(m.RIGHT); g.refresh_bounding_box(); return r(g.get_center())

    @case
    def set_points_after_shift():
        g = shifted(m.VMobject); g.set_points(np.array([[1.0, 2.0, 3.0]])); return r(g.get_center())

    @case
    def shift_then_clear_points():
        g = bind(m.VMobject()); g.set_points(np.array([[1.0, 2.0, 3.0]])); g.shift(m.RIGHT)
        g.clear_points(); return r(g.get_center())

    @case
    def match_points_empty_source():
        src = m.VMobject().shift(3 * m.UP); g = bind(m.VMobject())
        g.set_points(np.array([[1.0, 0, 0], [2.0, 0, 0], [3.0, 0, 0]]))
        g.match_points(src); return r(g.get_center())

    # Copies and states carry the position.
    @case
    def copy_keeps_position():
        g = shifted()
        return [r(g.copy().get_center()), r(g.deepcopy().get_center()),
                r(copy.deepcopy(g).get_center())]

    @case
    def copy_family_with_empty_child():
        inner = m.VGroup().shift(5 * m.RIGHT); g = bind(m.VGroup(inner, m.Square()))
        return r(g.copy()[0].get_center())

    @case
    def save_state_restore():
        g = shifted(); g.save_state(); g.shift(m.LEFT); g.restore(); return r(g.get_center())

    @case
    def generate_target():
        g = shifted(); g.generate_target(); g.target.shift(m.DOWN)
        return [r(g.target.get_center()), r(g.get_center())]

    @case
    def become_empty():
        g = fresh(); g.become(shifted()); return r(g.get_center())

    return out


def detached(mob):
    return mob


def scene_binder():
    # One Scene per run: `become` refuses endpoints from two Scenes.
    scene = m.Scene()

    def scene_bound(mob):
        scene.add(mob)
        return mob

    return scene_bound


class PointFreePositionTests(unittest.TestCase):
    maxDiff = None

    def assertMatchesReference(self, name, actual, expected):
        if isinstance(expected, dict):
            self.assertEqual(sorted(actual), sorted(expected), name)
            for key in expected:
                self.assertMatchesReference(f"{name}.{key}", actual[key], expected[key])
        elif isinstance(expected, bool):
            self.assertIs(actual, expected, name)
        else:
            np.testing.assert_allclose(np.asarray(actual, dtype=float),
                                       np.asarray(expected, dtype=float),
                                       rtol=0, atol=1e-5, err_msg=name)

    def check(self, bind):
        constructions = cases(bind)
        self.assertEqual(sorted(constructions), sorted(REFERENCE))
        for name, build in constructions.items():
            with self.subTest(case=name):
                self.assertMatchesReference(name, build(), REFERENCE[name])

    def test_detached_matches_the_reference(self):
        self.check(detached)

    def test_scene_bound_matches_the_reference(self):
        self.check(scene_binder())

    def test_bead_table(self):
        # The three rows fm-fc53 was filed with, each in a fresh object.
        self.assertMatchesReference("shift", m.VGroup().shift(2 * m.RIGHT).get_center(), [2, 0, 0])
        self.assertMatchesReference("move_to", m.VGroup().move_to(3 * m.UP).get_center(), [0, 3, 0])
        self.assertMatchesReference(
            "next_to", m.VGroup().next_to(m.Square(), m.DOWN).get_center(), [0, -1.25, 0])

    def test_bar_chart_empty_label_group(self):
        chart = m.BarChart([1, 2, 3])
        np.testing.assert_allclose(chart.bar_labels.get_center(), BAR_LABELS_CENTER,
                                   rtol=0, atol=1.5e-3)

    def test_points_bearing_family_box_is_unchanged_by_its_empty_members(self):
        # A family with points keeps its box over points: positioned empty
        # members never widen it, in either proxy state.
        for bind in (detached, scene_binder()):
            with self.subTest(bind=bind.__name__):
                far = m.VGroup().shift(100 * m.RIGHT)
                g = bind(m.VGroup(far, m.Square().shift(m.UP)))
                np.testing.assert_allclose(g.get_bounding_box(),
                                           [[-1, 0, 0], [0, 1, 0], [1, 2, 0]], atol=1e-6)
                g.rotate(m.PI / 2)
                np.testing.assert_allclose(g.get_bounding_box(),
                                           [[-1, 0, 0], [0, 1, 0], [1, 2, 0]], atol=1e-6)


# C-33 (fm-pointless-box-interpolation-hccu): a point-less family's box is
# its position, and interpolating it follows the path whatever was read
# before. The Reference interpolates the box but leaves its refresh flag set,
# so its results depend on an earlier read. Measured there with this script:
# the origin for the three played rows and the unread interpolate, (1, 0, 0)
# after a read, and origin then (0, 2, 0) for the stepped Transform. These
# are the ruled values, the same in both proxy states.
RULED_INTERPOLATION = {
    'transform_played': [0.0, 3.0, 0.0],
    'move_to_target_played': [-1.0, 0.0, 0.0],
    'animate_shift_played': [2.0, 0.0, 0.0],
    'interpolate_after_read': [1.0, 0.0, 0.0],
    'interpolate_unread': [1.0, 0.0, 0.0],
    'interpolate_after_shift': [1.0, 0.0, 0.0],
    'transform_stepped': [[0.0, 1.0, 0.0], [0.0, 2.0, 0.0]],
    'nested_family': {"root": [0.0, 0.0, 0.0], "child": [1.0, 0.0, 0.0]},
    'points_bearing_control': [1.0, 0.0, 0.0],
}


def interpolation_cases(bind, scene):
    def fresh():
        return bind(m.VGroup())

    def play(*animations):
        scene.play(*animations, run_time=1 / 15)

    out = {}

    def case(fn):
        out[fn.__name__] = fn
        return fn

    @case
    def transform_played():
        a = fresh(); play(m.Transform(a, m.VGroup().shift(3 * m.UP))); return r(a.get_center())

    @case
    def move_to_target_played():
        g = fresh(); g.generate_target(); g.target.shift(m.LEFT)
        play(m.MoveToTarget(g)); return r(g.get_center())

    @case
    def animate_shift_played():
        g = fresh(); play(g.animate.shift(2 * m.RIGHT)); return r(g.get_center())

    @case
    def interpolate_after_read():
        g = fresh(); g.get_center()
        g.interpolate(m.VGroup(), m.VGroup().shift(4 * m.RIGHT), 0.25); return r(g.get_center())

    @case
    def interpolate_unread():
        g = fresh(); g.interpolate(m.VGroup(), m.VGroup().shift(4 * m.RIGHT), 0.25)
        return r(g.get_center())

    @case
    def interpolate_after_shift():
        # The shift installs a cached box; the interpolated position wins.
        g = fresh(); g.shift(3 * m.LEFT)
        g.interpolate(m.VGroup(), m.VGroup().shift(4 * m.RIGHT), 0.25); return r(g.get_center())

    @case
    def transform_stepped():
        a = fresh(); animation = m.Transform(a, m.VGroup().shift(2 * m.UP))
        animation.begin(); animation.interpolate(0.5); half = r(a.get_center())
        animation.finish(); return [half, r(a.get_center())]

    def stepped(start, target, alpha):
        animation = m.Transform(bind(start), target, rate_func=m.linear)
        animation.begin()
        animation.interpolate(alpha)
        return start

    # Mobject.interpolate moves only its receiver; Transform interpolates
    # every family member.
    @case
    def nested_family():
        g = stepped(m.VGroup(m.VGroup()), m.VGroup(m.VGroup().shift(4 * m.RIGHT)), 0.25)
        return {"root": r(g.get_center()), "child": r(g[0].get_center())}

    @case
    def points_bearing_control():
        g = stepped(m.VGroup(m.Square()), m.VGroup(m.Square().shift(2 * m.RIGHT)), 0.5)
        return r(g.get_center())

    return out


class PointFreeInterpolation(unittest.TestCase):
    maxDiff = None
    assertMatchesReference = PointFreePositionTests.assertMatchesReference

    def check_interpolation(self, bind, scene):
        constructions = interpolation_cases(bind, scene)
        self.assertEqual(sorted(constructions), sorted(RULED_INTERPOLATION))
        for name, build in constructions.items():
            with self.subTest(case=name):
                self.assertMatchesReference(name, build(), RULED_INTERPOLATION[name])

    def test_detached_follows_the_ruling(self):
        self.check_interpolation(detached, m.Scene())

    def test_scene_bound_follows_the_ruling(self):
        scene = m.Scene()

        def scene_bound(mob):
            scene.add(mob)
            return mob

        self.check_interpolation(scene_bound, scene)

    def test_a_box_over_points_follows_an_animated_rotation(self):
        # Appendix C-43: the Reference's interpolate copies the target's cached box, which
        # the builder's rotate left stale, so its square keeps the unrotated box (+-0.95).
        square = m.Square(side_length=1.9)
        animation = square.animate.rotate(2.91).build()
        animation.begin()
        animation.interpolate(1.0)
        animation.finish()
        points = square.get_points()
        box = square.get_bounding_box()
        np.testing.assert_allclose(box[0], points.min(axis=0), atol=1e-5)
        np.testing.assert_allclose(box[2], points.max(axis=0), atol=1e-5)
        self.assertAlmostEqual(square.get_width(), 2.2854, places=3)

    def test_path_arc_moves_a_point_less_box_off_the_straight_line(self):
        start, end = np.array([1.0, 0.0, 0.0]), np.array([-1.0, 0.0, 0.0])
        expected = m.path_along_arc(m.PI)(np.array([start]), np.array([end]), 0.5)[0]
        self.assertGreater(abs(expected[1]), 0.5)
        a = m.VGroup().shift(start)
        animation = m.Transform(a, m.VGroup().shift(end), path_arc=m.PI)
        animation.begin()
        animation.interpolate(0.5)
        self.assertMatchesReference("path_arc_half", r(a.get_center()), r(expected))


class PointFreeGetterRefusals(unittest.TestCase):
    # fm-point-getter-exception-class-8vjgn: on a mobject without points the Reference's
    # getters reach throw_error_if_no_points (mobject.py:2188), a plain Exception naming the
    # method that checked. Classes and messages recorded on the pinned Reference; the
    # mobjects lose their points through become, as in the API fuzzer's s34-000083 and
    # s34-000123.
    REFERENCE = {
        "Circle.get_radius": "get_start",
        "Circle.get_start": "get_start",
        "Circle.get_end": "get_end",
        "Line.get_angle": "get_end",
        "Line.get_length": "get_start_and_end",
        "Line.get_start_and_end": "get_start_and_end",
        "Arc.get_start_angle": "get_start",
        "VMobject.get_start": "get_start",
    }

    def calls(self, bind):
        circle = bind(m.Circle(radius=0.73))
        circle.become(m.VCube(side_length=1.99))
        line = bind(m.Line(np.array([-0.51, 1.99, 0.0]), np.array([-2.45, 2.6, 0.0])))
        line.become(m.DashedLine(np.array([-1.84, 0.23, 0.0]), np.array([-2.59, 0.18, 0.0])))
        arc = bind(m.Arc(start_angle=0.3, angle=1.2))
        arc.become(m.VCube(side_length=1.0))
        return {
            "Circle.get_radius": circle.get_radius,
            "Circle.get_start": circle.get_start,
            "Circle.get_end": circle.get_end,
            "Line.get_angle": line.get_angle,
            "Line.get_length": line.get_length,
            "Line.get_start_and_end": line.get_start_and_end,
            "Arc.get_start_angle": arc.get_start_angle,
            "VMobject.get_start": bind(m.VMobject()).get_start,
        }

    def check(self, bind):
        calls = self.calls(bind)
        self.assertEqual(sorted(calls), sorted(self.REFERENCE))
        for name, call in calls.items():
            with self.subTest(getter=name):
                with self.assertRaises(Exception) as caught:
                    call()
                self.assertIs(type(caught.exception), Exception)
                self.assertEqual(
                    str(caught.exception),
                    f"Cannot call Mobject.{self.REFERENCE[name]} for a Mobject with no points")

    def test_detached_getters_refuse_like_the_reference(self):
        self.check(detached)

    def test_scene_bound_getters_refuse_like_the_reference(self):
        self.check(scene_binder())


class PointLessTargetAlignment(unittest.TestCase):
    # fm-pointless-target-alignment-b0y0v: the Reference's VMobject.align_points
    # (vectorized_mobject.py:970) starts a path at a point-less side's center before it pads,
    # so a transform toward a point-less root (a DashedLine, a VCube) collapses onto that root's
    # family center with the root's own style. The values are the pinned Reference's for the
    # API fuzzer's minimized programs s34-000953, s34-001408, s34-000762, s34-003940,
    # s34-001685 and s34-009260.
    REFERENCE = {
        "transform_to_bound_dashed": {
            "fill": "#888888", "n": 9, "first": [1.475, -0.645, 0], "family_len": 41},
        "replacement_to_dashed_copy": {
            "fill": "#888888", "n": 11, "first": [0.55, -0.52, 0], "family_len": 35},
        "replacement_to_vcube_copy": {
            "fill": "#29ABCA", "n": 33, "first": [0, 0, 0], "family_len": 7},
        "triangle_to_dashed": {
            "fill": "#888888", "n": 7, "first": [1.93, 0.57, 0], "family_len": 42},
        "polygon_to_dashed": {
            "fill": "#888888", "n": 7, "first": [-0.47, 0.025, 0], "family_len": 41},
        "square_to_arc_dashed_copy": {
            "fill": "#888888", "n": 23, "first": [1.0, 0.02, 0], "family_len": 20},
    }

    def play(self, scene, animation):
        scene.play(animation, run_time=1 / 15)

    def cases(self):
        scene = m.Scene()
        out = {}
        rect = m.Rectangle(width=3.15, height=0.39)
        dashed = m.DashedLine(np.array([2.64, -2.26, 0.0]), np.array([0.31, 0.97, 0.0]))
        self.play(scene, dashed.animate.scale(0.6).scale(1.96).scale(1.76))
        self.play(scene, m.Transform(rect, dashed))
        out["transform_to_bound_dashed"] = rect
        scene.clear()
        dashed = m.DashedLine(np.array([0.99, -2.15, 0.0]), np.array([0.11, 1.11, 0.0]))
        scene.add(dashed)
        pentagon = m.RegularPolygon(n=5)
        self.play(scene, m.ReplacementTransform(pentagon, dashed.copy()))
        out["replacement_to_dashed_copy"] = pentagon
        scene.clear()
        cube = m.VCube(side_length=1.72)
        scene.add(cube)
        dot = m.Dot(point=np.array([0.47, -0.02, 0.0]))
        self.play(scene, m.ReplacementTransform(dot, cube.copy()))
        out["replacement_to_vcube_copy"] = dot
        scene.clear()
        triangle = m.Triangle()
        self.play(scene, m.Transform(
            triangle, m.DashedLine(np.array([2.91, 2.33, 0.0]), np.array([0.95, -1.19, 0.0]))))
        out["triangle_to_dashed"] = triangle
        scene.clear()
        polygon = m.Polygon(np.array([-0.44, 2.81, 0.0]), np.array([2.15, -1.53, 0.0]),
                            np.array([-1.23, 1.05, 0.0]))
        self.play(scene, m.Transform(
            polygon, m.DashedLine(np.array([1.52, 0.22, 0.0]), np.array([-2.46, -0.17, 0.0]))))
        out["polygon_to_dashed"] = polygon
        scene.clear()
        dashed = m.DashedLine(np.array([1.93, -2.88, 0.0]), np.array([0.46, -1.78, 0.0]))
        dashed.become(m.Arc(start_angle=0.02, angle=3.1, radius=1.0, n_components=11))
        square = m.RegularPolygon(n=4)
        self.play(scene, m.ReplacementTransform(square, dashed.copy()))
        out["square_to_arc_dashed_copy"] = square
        return out

    def test_transforms_toward_point_less_roots_match_the_reference(self):
        cases = self.cases()
        self.assertEqual(sorted(cases), sorted(self.REFERENCE))
        for name, mob in cases.items():
            expected = self.REFERENCE[name]
            with self.subTest(case=name):
                points = mob.get_points()
                self.assertEqual(mob.get_fill_color(), expected["fill"])
                self.assertEqual(len(points), expected["n"])
                np.testing.assert_allclose(points[0], expected["first"], atol=1e-3)
                self.assertEqual(len(mob.get_family()), expected["family_len"])
                self.assertTrue(all(type(member) is type(mob) for member in mob.get_family()))


_suite = unittest.TestSuite([
    unittest.defaultTestLoader.loadTestsFromTestCase(PointFreePositionTests),
    unittest.defaultTestLoader.loadTestsFromTestCase(PointFreeInterpolation),
    unittest.defaultTestLoader.loadTestsFromTestCase(PointFreeGetterRefusals),
    unittest.defaultTestLoader.loadTestsFromTestCase(PointLessTargetAlignment),
])
assert _suite.countTestCases() == 12, "point-free position inventory changed"
_result = unittest.TextTestRunner(verbosity=2).run(_suite)
if not _result.wasSuccessful():
    raise AssertionError("point-free position acceptance failed")
