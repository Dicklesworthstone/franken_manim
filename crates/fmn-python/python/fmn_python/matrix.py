"""Live Matrix authoring over the existing native mobjects and Scribe glyphs.

No geometry, font parser or animation interpolation lives here. These are
Python object-model operations: overridable entry conversion, grouping, and
identity-preserving replacement of already-built native entries.
"""
from __future__ import annotations

import math
import operator
from typing import Any

_MAX_ENTRIES = 4096


def _finite(value: Any, name: str, *, nonnegative: bool = False) -> float:
    value = float(value)
    if not math.isfinite(value) or (nonnegative and value < 0):
        qualifier = "finite and nonnegative" if nonnegative else "finite"
        raise ValueError(f"{name} must be {qualifier}")
    return value


def _index(value: Any, length: int) -> int | None:
    if value is None:
        return None
    value = operator.index(value)
    return value % length if -length <= value < length else None


def _entry_config(matrix: Any, config: dict, *, decimal: bool) -> dict:
    result = dict(getattr(matrix, "_matrix_entry_style", {}))
    result["font_size"] = getattr(matrix, "_matrix_entry_font_size", 48.0)
    if decimal:
        result["num_decimal_places"] = getattr(matrix, "_matrix_decimal_places", 2)
    result.update(config)
    return result


def _bind(cls: type, name: str, method: Any) -> None:
    # Do not use wraps on schema stubs: that would copy placeholder provenance
    # onto a real implementation. Class identity and every module alias stay.
    method.__name__ = name
    method.__qualname__ = cls.__qualname__ + "." + name
    method.__module__ = cls.__module__
    setattr(cls, name, method)


def install_matrix(native: Any) -> None:
    if vars(native).get("_FMN_MATRIX_INSTALLED", False):
        return
    Matrix, DecimalMatrix = native.Matrix, native.DecimalMatrix
    VMobject, VGroup = native.VMobject, native.VGroup
    Tex, DecimalNumber = native.Tex, native.DecimalNumber
    np = vars(native)["_np"]

    def element_to_mobject(self, element, **config):
        if isinstance(element, VMobject):
            return element
        if isinstance(element, (float, complex, np.floating, np.complexfloating)):
            return DecimalNumber(element, **_entry_config(self, config, decimal=True))
        return Tex(str(element), **_entry_config(self, config, decimal=False))

    def decimal_element_to_mobject(self, element, **config):
        return DecimalNumber(element, **_entry_config(self, config, decimal=True))

    def create_brackets(self, rows, v_buff, h_buff):
        count = len(rows)
        if not 0 < count <= _MAX_ENTRIES:
            raise ValueError("matrix brackets require 1..4096 rows")
        v_buff = _finite(v_buff, "bracket_v_buff")
        h_buff = _finite(h_buff, "bracket_h_buff")
        height = _finite(rows.get_height() + v_buff, "bracket height", nonnegative=True)
        source = r"\left[\begin{array}{c}" + count * r"\quad \\" + r"\end{array}\right]"
        brackets = Tex(source)
        if len(brackets) < 2:
            raise RuntimeError("native matrix delimiter typesetting did not produce a bracket pair")
        brackets.set_height(height)
        middle = len(brackets) // 2
        left, right = brackets[:middle], brackets[middle:]
        left.next_to(rows, native.LEFT, h_buff)
        right.next_to(rows, native.RIGHT, h_buff)
        return VGroup(left, right)

    def swap_entry_for_dots(self, entry, dots):
        if not isinstance(entry, VMobject) or not isinstance(dots, VMobject):
            raise TypeError("matrix ellipsis replacement requires VMobject entries and dots")
        dots.move_to(entry)
        # Native become keeps the live entry handle; replacing the Python list
        # slot here would strand animations, aliases and external references.
        entry.become(dots)
        for index, element in enumerate(self.elements):
            if element is entry:
                del self.elements[index]
                break
        if not any(element is entry for element in self.ellipses):
            self.ellipses.append(entry)
        # The Reference helper returns None; the batch operation returns self.

    def swap_entries_for_ellipses(
        self, row_index=None, col_index=None, height_ratio=0.65, width_ratio=0.4,
    ):
        rows, columns = self.get_rows(), self.get_columns()
        n_rows, n_cols = len(rows), len(columns)
        if not n_rows or not n_cols or n_rows * n_cols > _MAX_ENTRIES:
            raise ValueError("matrix ellipses require a nonempty grid of at most 4096 entries")
        row_index, col_index = _index(row_index, n_rows), _index(col_index, n_cols)
        if row_index is None and col_index is None:
            return self
        height_ratio = _finite(height_ratio, "height_ratio", nonnegative=True)
        width_ratio = _finite(width_ratio, "width_ratio", nonnegative=True)
        height = _finite(height_ratio * rows.get_height() / n_rows, "ellipsis height", nonnegative=True)
        width = _finite(width_ratio * columns.get_width() / n_cols, "ellipsis width", nonnegative=True)
        # Complete native typesetting and scaling before touching live cells.
        # A font/capability/budget error cannot leave half of a row replaced.
        replacements = []
        if row_index is not None:
            for column in columns:
                dots = Tex(r"\vdots")
                dots.set_height(height)
                replacements.append((column[row_index], dots))
        if col_index is not None:
            for row in rows:
                dots = Tex(r"\hdots")
                dots.set_width(width)
                replacements.append((row[col_index], dots))
        for entry, dots in replacements:
            self.swap_entry_for_dots(entry, dots)
        if row_index is not None and col_index is not None:
            rows[row_index][col_index].rotate(-45 * native.DEG)
        return self

    def set_column_colors(self, *colors):
        for column, color in zip(self.get_columns(), colors):
            column.set_color(color)
        return self

    def add_background_to_entries(self):
        for entry in self.get_entries():
            entry.add_background_rectangle()
        return self

    for name, method in (
        ("element_to_mobject", element_to_mobject),
        ("create_brackets", create_brackets),
        ("swap_entry_for_dots", swap_entry_for_dots),
        ("swap_entries_for_ellipses", swap_entries_for_ellipses),
        ("set_column_colors", set_column_colors),
        ("add_background_to_entries", add_background_to_entries),
    ):
        _bind(Matrix, name, method)
    _bind(DecimalMatrix, "element_to_mobject", decimal_element_to_mobject)
    native._FMN_MATRIX_INSTALLED = True
