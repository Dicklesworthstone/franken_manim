"""Native ruled tables for data-driven scenes, without a host pandas install.

CSV goes through Atlas/frankenpandas; literal string grids and CSV share the
same Scribe table builder. No parser, font engine or layout solver lives here.
"""
from __future__ import annotations

import itertools
import math
import operator

import manimlib as m

_MAX_CELLS = 4096
_MAX_BYTES = 262_144


def _items(values, limit, name):
    if isinstance(values, (str, bytes)):
        raise TypeError(name + " must be an iterable, not a string")
    result = list(itertools.islice(iter(values), limit + 1))
    if len(result) > limit:
        raise ValueError(name + " exceeds the 4096-cell table budget")
    return result


def _grid(headers, rows):
    names = _items(headers, _MAX_CELLS // 2, "headers")
    if not names:
        raise ValueError("table requires at least one header")
    body = _items(rows, _MAX_CELLS // len(names) - 1, "rows")
    if not body:
        raise ValueError("table requires at least one body row")
    body = [_items(row, len(names), "row") for row in body]
    if any(len(row) != len(names) for row in body):
        raise ValueError("table rows must match the header count")
    size = 0
    for row in [names, *body]:
        for value in row:
            if not isinstance(value, str):
                raise TypeError("literal table cells must be strings; CSV performs native type inference")
            size += len(value.encode("utf-8"))
            if size > _MAX_BYTES:
                raise ValueError("table exceeds 262144 UTF-8 bytes")
    return tuple(names), tuple(tuple(row) for row in body)


def _index(value, length):
    value = operator.index(value)
    if not -length <= value < length:
        raise IndexError("table index out of range")
    return value % length


class TableMobject(m.VGroup):
    """A native table whose cells and rules are independently animatable.

    Body indices are zero-based (negative indices are supported). A column
    may also be named when its header is unique. Returned groups contain live
    native objects, while ``headers`` and ``values`` are immutable text tuples.
    """
    def __init__(self, headers, rows, *, font_size=24, color=m.WHITE,
                 header_color=m.BLUE_B, rule_color=m.GREY_B, **kwargs):
        names, body = _grid(headers, rows)
        size = float(font_size)
        if not math.isfinite(size) or not 0 < size <= 10000:
            raise ValueError("table font_size must be finite and in (0, 10000]")
        for value in (color, header_color, rule_color):
            rgb = m.color_to_rgb(value)
            if not all(math.isfinite(v) and 0 <= v <= 1 for v in rgb):
                raise ValueError("table colors must have finite RGB components in [0,1]")
        builder = getattr(m, "_build_table", None)
        if not callable(builder):
            raise m._CapabilityError("this native wheel does not provide table construction")
        raw = m.VMobject()
        specs = builder(raw, m._native_shell_factory, list(names), [list(row) for row in body])
        m._hang_native_children(raw, specs)
        count = len(names) + len(body)
        if len(raw) != count + len(names) * (len(body) + 1):
            raise RuntimeError("native table family does not match its declared grid")
        children = tuple(raw.submobjects)
        raw.remove(*children)
        rules = m.VGroup(*children[:count])
        cells = m.VGroup(*children[count:])
        # The native outline is the placement authority, including empty cells.
        # Three invisible points retain the authored affine chart through all
        # ordinary mobject transformations, copies and scene checkpoints.
        anchors = m.VGroup(*(m.VectorizedPoint(rules[0].get_corner(d)) for d in (m.DL, m.DR, m.UL)))
        super().__init__(rules, cells, anchors, **kwargs)
        self._table_rules, self._table_cells, self._table_anchors = rules, cells, anchors
        self._table_headers, self._table_values = names, body
        self._table_layout_size = (rules[0].get_width(), rules[0].get_height())
        self._table_font_size = size
        self._table_colors = (color, header_color, rule_color)
        cells.set_color(color)
        self.get_headers().set_color(header_color)
        rules.set_stroke(rule_color)
        self.scale(size / 48, about_point=m.ORIGIN)
        # Invisible chart anchors must not determine the visible table bounds.
        self.shift(-rules[0].get_center())

    @classmethod
    def from_csv(cls, text, *, separator=",", **kwargs):
        if not isinstance(text, str):
            raise TypeError("table CSV must be text; read files explicitly in the host")
        if len(text.encode("utf-8")) > _MAX_BYTES:
            raise ValueError("table CSV exceeds 262144 UTF-8 bytes")
        if not isinstance(separator, str) or len(separator) != 1:
            raise ValueError("table CSV separator must be one character")
        parser = getattr(m, "_table_from_csv", None)
        if not callable(parser):
            raise m._CapabilityError("this native wheel does not provide CSV tables")
        names, body = parser(text, separator)
        return cls(names, body, **kwargs)

    @property
    def headers(self):
        return self._table_headers

    @property
    def values(self):
        return self._table_values

    @property
    def shape(self):
        return len(self.values), len(self.headers)

    def _column(self, column):
        if isinstance(column, str):
            matches = [i for i, name in enumerate(self.headers) if name == column]
            if len(matches) != 1:
                raise KeyError("table column name is missing or ambiguous: " + column)
            return matches[0]
        return _index(column, len(self.headers))

    def get_cell(self, row, column):
        return self._table_cells[(_index(row, len(self.values)) + 1) * len(self.headers) + self._column(column)]

    def get_headers(self):
        return m.VGroup(*self._table_cells.submobjects[:len(self.headers)])

    def get_rows(self):
        return m.VGroup(*(m.VGroup(*(self.get_cell(row, col) for col in range(self.shape[1])))
                          for row in range(self.shape[0])))

    def get_columns(self):
        return m.VGroup(*(m.VGroup(*(self.get_cell(row, col) for row in range(self.shape[0])))
                          for col in range(self.shape[1])))

    def get_rules(self):
        return m.VGroup(*self._table_rules.submobjects)
