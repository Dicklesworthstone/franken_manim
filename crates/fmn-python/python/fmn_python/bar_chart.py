"""Zero-safe signed bar updates using native rectangles and live chart geometry."""
from __future__ import annotations

from functools import wraps
import inspect
import itertools
import math

from .graphing import _bind_method
from .movement import _changed, _class_implementation, _implementation, _protocols

_MAX_BARS = 4096
_MAX_PARENTS = 65_536
_BUSY = '_fmn_bar_chart_updating'
_UNSET = object()


def _values(values, limit):
    result = tuple(itertools.islice(iter(values), limit + 1))
    if len(result) > limit:
        raise ValueError('bar values exceed the chart or its 4096-bar budget')
    result = tuple(float(value) for value in result)
    if not all(math.isfinite(value) for value in result):
        raise ValueError('bar values must be finite real numbers')
    return result


def install_bar_chart(native):
    g = vars(native)
    if g.get('_FMN_BAR_CHART_INSTALLED', False):
        return
    Chart, np = g['BarChart'], g['_np']
    original = Chart.__init__
    signature = inspect.signature(original)
    # Take only detached numeric templates from the shipped native rectangle.
    # The bottom edge remains nondegenerate when height is zero, so no inverse
    # of the previous height (or guessed minimum height) is ever required.
    unit = g['Rectangle'](width=1, height=1).get_points().copy()
    uv = unit[:, :2] + .5
    left = int(np.flatnonzero(np.all(uv == (0, 0), axis=1))[0])
    right = int(np.flatnonzero(np.all(uv == (1, 0), axis=1))[0])
    xs, ys = uv[:, :1].copy(), uv[:, 1:2].copy()
    pad = float(g['_MED_LARGE_BUFF'])
    Mobject, Rectangle, Line = g['Mobject'], g['Rectangle'], g['Line']
    native_change = g.get('_change_bar_values')
    resize_order = g['resize_preserving_order']
    # Only these shipping line/rectangle protocols match the native kernel.
    # A different shipped shape is still entitled to its own get/set hooks.
    bar_protocols = _protocols({'Rectangle': Rectangle}, Mobject, (
        'get_points', 'set_points', 'resize_points', 'refresh_bounding_box',
        'get_family', '_is_bound', '_refresh_point_free_anchors',
        '_refresh_vmobject_path_metadata', '_bake_placement', '_data_array',
        'data', 'points', '__getattribute__', '__getattr__', '__setattr__', '__dict__',
    ))
    axis_protocols = _protocols({'Line': Line}, Mobject, (
        'get_start', 'get_end', '_get_start', '_get_end', 'has_tip', 'has_start_tip',
        '__getattribute__', '__getattr__', '__dict__',
    ))
    parent_protocols = _protocols({'Mobject': Mobject}, Mobject, (
        'refresh_bounding_box', 'get_family', '_is_bound', '_refresh_point_free_anchors',
        '__getattribute__', '__getattr__', '__dict__',
    ))
    chart_protocols = _protocols({'BarChart': Chart}, Mobject, (
        '__getattribute__', '__getattr__', '__dict__',
    ))

    def native_parents(selected, memo):
        # refresh_bounding_box walks Python parent links even for detached
        # objects. Admit that entire protocol without invoking descriptors,
        # then let the same native crossing reset their point-free anchors.
        if len(selected) > _MAX_PARENTS:
            return None
        stack = [(bar, False) for bar in reversed(selected)]
        seen, active, parents = set(), set(), {}
        while stack:
            member, leaving = stack.pop()
            marker = id(member)
            if leaving:
                active.remove(marker)
                seen.add(marker)
                continue
            if marker in active:
                return None
            if marker in seen:
                continue
            if (len(seen) + len(active) >= _MAX_PARENTS
                    or not isinstance(member, Mobject)
                    or _changed(member, parent_protocols, memo)
                    or _class_implementation(type(member), 'parents', memo) is not None):
                return None
            owners = vars(member).get('parents')
            if type(owners) is not list:
                return None
            # Bound the pending expansion before enqueueing an authored broad
            # parent list. Counting duplicates conservatively keeps admission
            # allocation bounded without traversing that list first.
            if (len(stack) + len(owners) + 1 > _MAX_PARENTS
                    or len(parents) + len(owners) > _MAX_PARENTS):
                return None
            active.add(marker)
            stack.append((member, True))
            for parent in reversed(owners):
                parents[id(parent)] = parent
                stack.append((parent, False))
        return tuple(parents.values())

    def try_native(self, bars, numbers, maximum):
        if not numbers or native_change is None or g['resize_preserving_order'] is not resize_order:
            return False
        memo = {}
        if (_changed(self, chart_protocols, memo)
                or _class_implementation(type(self), 'y_axis', memo) is not None
                or _class_implementation(type(self), '_fmn_bar_height_fraction', memo) is not None):
            return False
        axis = vars(self).get('y_axis')
        fraction = vars(self).get('_fmn_bar_height_fraction')
        if (not isinstance(axis, Line) or _changed(axis, axis_protocols, memo)
                or inspect.getattr_static(axis, 'tip', _UNSET) is not _UNSET
                or inspect.getattr_static(axis, 'start_tip', _UNSET) is not _UNSET
                or type(fraction) is not float or not math.isfinite(fraction) or fraction <= 0):
            return False
        selected = bars[:len(numbers)]
        for bar in selected:
            if (not isinstance(bar, Rectangle) or _changed(bar, bar_protocols, memo)
                    or _class_implementation(type(bar), 'pointlike_data_keys', memo)
                       is not _class_implementation(Rectangle, 'pointlike_data_keys', memo)
                    or vars(bar).get('_data_defaults') is None
                    or _class_implementation(type(bar), 'needs_new_unit_normal', memo) is not None):
                return False
            keys = _implementation(bar, 'pointlike_data_keys')
            if (type(keys) not in (list, tuple) or len(keys) != 1
                    or type(keys[0]) is not str or keys[0] != 'point'):
                return False
        parents = native_parents(selected, memo)
        if parents is None or not native_change(axis, selected, numbers, parents, maximum, fraction):
            return False
        for bar in selected:
            # Stock VMobject.set_points sets this ordinary cache flag after
            # native joint-angle maintenance. Descriptor/setattr overrides
            # already selected the authored path above.
            vars(bar)['needs_new_unit_normal'] = True
        return True

    @wraps(original)
    def initialize(self, *args, **kwargs):
        if self._is_bound():
            raise RuntimeError('BarChart construction requires a detached target')
        bound = signature.bind(self, *args, **kwargs)
        values = _values(bound.arguments['values'], _MAX_BARS)
        if not values:
            raise ValueError('BarChart requires at least one value')
        maximum = bound.arguments.get('max_value', 1)
        if maximum is None:
            maximum = max(abs(value) for value in values) or 1.
        else:
            maximum = float(maximum)
        height, width = (float(bound.arguments.get(name, default))
                         for name, default in (('height', 4), ('width', 6)))
        if not all(math.isfinite(value) and value > 0 for value in (maximum, height, width)):
            raise ValueError('bar chart height, width and max_value must be finite and positive')
        max_coordinate = float(np.finfo(np.float32).max)
        if (max(height, width) > max_coordinate
                or any(not math.isfinite(value / maximum * height)
                       or abs(value / maximum * height) > max_coordinate for value in values)):
            raise ValueError('bar chart dimensions must produce finite f32-representable geometry')
        bound.arguments.update(values=tuple(abs(value) for value in values),
                               max_value=maximum, height=height, width=width)
        original(*bound.args, **bound.kwargs)
        self._fmn_bar_height_fraction = height / (height + pad)
        if any(value < 0 for value in values):
            change(self, values)
            self.center()

    def change(self, values):
        """Update a prefix of bars, preserving baselines through zero/sign changes.

        Height follows the live chart y-axis, including affine transforms. Bar
        identities, widths, children, style records and updaters are retained.
        Labels are independent annotations and keep their authored placement.
        """
        if vars(self).get(_BUSY, False):
            raise RuntimeError('bar chart updates cannot reenter themselves')
        vars(self)[_BUSY] = True
        try:
            bars = tuple(self.bars)
            if len(bars) > _MAX_BARS:
                raise ValueError('bar chart exceeds its 4096-bar budget')
            numbers = _values(values, len(bars))
            maximum = float(self.max_value)
            if not math.isfinite(maximum) or maximum <= 0:
                raise ValueError('bar chart max_value must be finite and positive')
            if try_native(self, bars, numbers, maximum):
                return None
            height = (np.asarray(self.y_axis.get_end(), dtype=float)
                      - np.asarray(self.y_axis.get_start(), dtype=float))
            height *= self._fmn_bar_height_fraction
            if height.shape != (3,) or not np.isfinite(height).all():
                raise ValueError('bar chart y-axis must have finite three-dimensional geometry')
            planned = []
            for bar, number in zip(bars, numbers):
                if tuple(bar.pointlike_data_keys) != ('point',):
                    raise TypeError('bar updates require the rectangle pointlike schema')
                points = np.asarray(bar.get_points(), dtype=float)
                if points.shape != unit.shape or not np.isfinite(points).all():
                    raise ValueError('bar updates require the original rectangular point topology')
                with np.errstate(over='ignore', invalid='ignore', divide='ignore'):
                    target = (points[left] + xs * (points[right] - points[left])
                              + ys * (height * (number / maximum)))
                if not np.isfinite(target).all() or np.any(np.abs(target) > np.finfo(np.float32).max):
                    raise ValueError('bar height must produce finite f32-representable geometry')
                planned.append((bar, target))
            # All values, topologies and target records are checked before the
            # first publication. Native fixed-size writes keep live views valid.
            for bar, target in planned:
                bar.set_points(target)
        finally:
            vars(self).pop(_BUSY, None)
        return None

    Chart.__init__ = initialize
    _bind_method(Chart, 'change_bar_values', change)
    g['_FMN_BAR_CHART_INSTALLED'] = True
