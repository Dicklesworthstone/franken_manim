"""Source-addressable Markdown scenes using Atlas/fmd and Scribe, not HTML.

The host retains source catalogs and edit ownership. All parsing, typography,
code highlighting, table layout, geometry and animation use their native owners.
"""
from __future__ import annotations

from collections import defaultdict, deque
import math
import operator

import manimlib as m
import numpy as np

from .invocation import InvocationGuard
from .scene_execution import _note

_EDITS = InvocationGuard()
_MAX_BYTES = 32_768


def _source(value):
    if not isinstance(value, str):
        raise TypeError("Markdown source must be text; read files explicitly")
    value = str.__str__(value)
    if len(value) > _MAX_BYTES or len(value.encode('utf-8')) > _MAX_BYTES:
        raise ValueError("Markdown source exceeds 32768 UTF-8 bytes")
    return value


def _index(value, length):
    value = operator.index(value)
    if not -length <= value < length:
        raise IndexError("Markdown block index out of range")
    return value % length


def _discrete(method):
    def refuse(*args, **kwargs):
        raise m._CapabilityError("Markdown edits are discrete; use animate_source(...) for a morph")
    method._override_animate = refuse
    return method


class MarkdownMobject(m.VGroup):
    """Native Markdown with independently animatable, source-addressable blocks.

    ``block_ranges`` are UTF-8 byte intervals from fmd. ``select_source`` uses
    Python character offsets and returns intersecting whole blocks, not glyphs.
    The document is centered initially; edits retain its authored upper-left
    affine frame. Lists/quotes flatten with prefixes; there is no browser, asset
    fetch, line wrapping or inline math renderer in this tier.
    """
    def __init__(self, source, *, font_size=24, theme='monokai', block_gap=.45, **kwargs):
        source = _source(source)
        size, gap = float(font_size), float(block_gap)
        if not math.isfinite(size) or not 0 < size <= 10000:
            raise ValueError("Markdown font_size must be finite and in (0, 10000]")
        if not math.isfinite(gap) or not 0 <= gap <= 1000:
            raise ValueError("Markdown block_gap must be finite and in [0, 1000]")
        if not isinstance(theme, str):
            raise TypeError("Markdown theme must be a native code-theme name")
        builder = getattr(m, '_build_markdown', None)
        if not callable(builder):
            raise m._CapabilityError("this native wheel does not provide Markdown construction")
        raw = m.VMobject()
        specs, ranges, kinds = builder(raw, m._native_shell_factory, source, size, theme, gap)
        m._hang_native_children(raw, specs)
        if len(raw) != len(ranges) or len(raw) != len(kinds):
            raise RuntimeError("native Markdown source catalog disagrees with its block family")
        width, height = raw.get_width(), raw.get_height()
        width, height = width if width > 0 else 1.0, height if height > 0 else 1.0
        children = tuple(raw.submobjects)
        raw.remove(*children)
        blocks = m.VGroup()
        for content in children:
            anchor = m.VectorizedPoint(content.get_corner(m.UL))
            block = m.VGroup(content, anchor)
            block._markdown_content, block._markdown_anchor = content, anchor
            blocks.add(block)
        # Three invisible native points carry placement through copy, animation
        # and checkpoints. Empty documents have a minimal, nondegenerate frame.
        anchors = m.VGroup(*(m.VectorizedPoint(p) for p in
                            ((0, 0, 0), (width, 0, 0), (0, -height, 0))))
        super().__init__(blocks, anchors, **kwargs)
        self._markdown_blocks, self._markdown_anchors = blocks, anchors
        self._markdown_source = source
        self._markdown_ranges = tuple(tuple(span) for span in ranges)
        self._markdown_kinds = tuple(kinds)
        self._markdown_size = (width, height)
        self._markdown_options = (size, theme, gap)
        self.shift(-self.get_center())

    @property
    def source(self):
        return self._markdown_source

    @property
    def block_ranges(self):
        return self._markdown_ranges

    @property
    def block_kinds(self):
        return self._markdown_kinds

    def get_blocks(self):
        return m.VGroup(*self._markdown_blocks.submobjects)

    def get_block(self, index):
        return self._markdown_blocks[_index(index, len(self.block_ranges))]

    def get_block_source(self, index):
        start, end = self.block_ranges[_index(index, len(self.block_ranges))]
        return self.source.encode('utf-8')[start:end].decode('utf-8')

    def select_source(self, start, end):
        """Select live whole blocks intersecting a half-open character interval."""
        start, end = operator.index(start), operator.index(end)
        if not 0 <= start <= end <= len(self.source):
            raise ValueError("source selection must satisfy 0 <= start <= end <= len(source)")
        a, b = len(self.source[:start].encode('utf-8')), len(self.source[:end].encode('utf-8'))
        return m.VGroup(*(block for block, (low, high) in zip(self._markdown_blocks, self.block_ranges)
                          if a < b and low < b and a < high))

    def select_text(self, text, *, occurrence=0):
        """Select blocks for one literal occurrence in the original Markdown."""
        text, occurrence = _source(text), operator.index(occurrence)
        if not text or occurrence < 0:
            raise ValueError("text must be nonempty and occurrence nonnegative")
        start, offset = -1, 0
        for _ in range(occurrence + 1):
            start = self.source.find(text, offset)
            if start < 0:
                raise ValueError("Markdown source occurrence not found")
            offset = start + len(text)
        return self.select_source(start, offset)

    def _structure(self):
        if (self._markdown_blocks not in self.submobjects or self._markdown_anchors not in self.submobjects
                or len(self._markdown_blocks) != len(self.block_ranges)
                or len(self.block_ranges) != len(self.block_kinds) or len(self._markdown_anchors) != 3):
            raise RuntimeError("Markdown-owned block/catalog structure was edited directly")
        for block in self._markdown_blocks:
            if (block._markdown_content not in block.submobjects
                    or block._markdown_anchor not in block.submobjects):
                raise RuntimeError("Markdown block content or anchor was removed")

    def _stamp(self):
        self._structure()
        family = self.get_family()
        for obj in family:
            obj.get_points()
        return (self.source, self.block_ranges, self.block_kinds, self._markdown_size, self._markdown_options,
                tuple((id(obj), id(getattr(obj, '_scene', None)), tuple(map(id, obj.submobjects)),
                       obj.data.dtype.descr, obj.data.tobytes()) for obj in family))

    def _chart(self):
        a, b, c = (anchor.get_center() for anchor in self._markdown_anchors)
        width, height = self._markdown_size
        ex, ey = (b-a)/width, (a-c)/height
        normal = np.cross(ex, ey)
        area, scale = np.linalg.norm(normal), np.linalg.norm(ex) * np.linalg.norm(ey)
        if not np.isfinite([a, ex, ey]).all() or not math.isfinite(scale) or area <= 1e-7 * scale or scale == 0:
            raise ValueError("Markdown reflow requires a finite, nondegenerate affine frame")
        return a, np.column_stack((ex, ey, normal / area))

    def _prepare(self, source):
        if vars(self).get('_is_animating', False) or vars(self).get('_markdown_partial', False):
            raise RuntimeError("finish or restore the active/partial Markdown animation before editing")
        if any(getattr(obj, 'locked_data_keys', ()) for obj in self.get_family()):
            raise RuntimeError("release Markdown record locks before editing")
        before = self._stamp()
        source = _source(source)
        if source == self.source:
            return None
        origin, matrix = self._chart()
        size, theme, gap = self._markdown_options
        candidate = MarkdownMobject(source, font_size=size, theme=theme, block_gap=gap)
        candidate.shift(-candidate._markdown_anchors[0].get_center())
        candidate.apply_matrix(matrix, about_point=m.ORIGIN)
        candidate.shift(origin)
        if self._stamp() != before or vars(self).get('_is_animating', False):
            raise RuntimeError("Markdown changed during native document preparation")
        return candidate

    def _keys(self):
        return [(kind, self.get_block_source(i).rstrip('\r\n')) for i, kind in enumerate(self.block_kinds)]

    def _publish(self, candidate, *, positional=False):
        old = tuple(self._markdown_blocks.submobjects)
        catalog = defaultdict(deque)
        for i, key in enumerate(self._keys()):
            catalog[key].append(i)
        matches, used = {}, set()
        keys = candidate._keys()
        for i, key in enumerate(keys):
            if positional and i < len(old):
                matches[i] = i
                used.add(i)
            elif catalog[key]:
                matches[i] = catalog[key].popleft()
                used.add(matches[i])
        # Retain edited same-kind block wrappers as well as exact matches. The
        # exact matches are reserved first so insertion cannot steal their IDs.
        for i, key in enumerate(keys):
            if i not in matches and i < len(old) and i not in used and self.block_kinds[i] == key[0]:
                matches[i] = i
                used.add(i)
        old_keys, blocks = self._keys(), []
        for i, new in enumerate(candidate._markdown_blocks):
            if i not in matches:
                blocks.append(new)
                continue
            j = matches[i]
            block = old[j]
            delta = new._markdown_anchor.get_center() - block._markdown_anchor.get_center()
            if not positional and old_keys[j] == keys[i]:
                block.shift(delta)  # Keep authored styling, glyph IDs and attached views.
            else:
                for annotation in tuple(block.submobjects):
                    if annotation is not block._markdown_content and annotation is not block._markdown_anchor:
                        annotation.shift(delta)
                block._markdown_content.set_points(new._markdown_content.get_points())
                block._markdown_content.set_submobjects(tuple(new._markdown_content.submobjects))
                block._markdown_anchor.set_points(new._markdown_anchor.get_points())
            blocks.append(block)
        self._markdown_blocks.set_submobjects(blocks)
        for old_anchor, new_anchor in zip(self._markdown_anchors, candidate._markdown_anchors):
            old_anchor.set_points(new_anchor.get_points())
        self._markdown_source, self._markdown_ranges = candidate.source, candidate.block_ranges
        self._markdown_kinds, self._markdown_size = candidate.block_kinds, candidate._markdown_size
        vars(self).pop('_markdown_partial', None)
        return self

    @_discrete
    def set_source(self, source):
        """Reflow a document; exact unchanged blocks keep their native identity."""
        with _EDITS.hold(self, message="Markdown source replacement cannot reenter the same document"):
            candidate = self._prepare(source)
            if candidate is not None:
                self._publish(candidate)
        return self

    def animate_source(self, source, **animation_config):
        """Morph equal-count block documents; inserts/removals use set_source.

        Source metadata is committed only at a complete endpoint. The native
        Transform owns easing, glyph alignment, sub-alpha and the scene clock.
        """
        return MarkdownSourceAnimation(self, _source(source), **animation_config)


class MarkdownSourceAnimation(m.Transform):
    """Source edit over the ordinary native glyph Transform lifecycle."""
    def __init__(self, document, source, **kwargs):
        if not isinstance(document, MarkdownMobject):
            raise TypeError("Markdown animation requires a MarkdownMobject")
        self._source_target = _source(source)
        self._source_initial = None
        self._source_alphas = []
        self._source_owned = False
        super().__init__(document, None, **kwargs)

    def begin(self):
        if self._source_owned:
            self.abort()
        self._source_initial = None
        with _EDITS.hold(self.mobject, message="Markdown animation preparation cannot reenter its document"):
            return self._begin_source()

    def _begin_source(self):
        if vars(self.mobject).get('_is_animating', False) or vars(self.mobject).get('_markdown_partial', False):
            raise RuntimeError("finish or restore the active/partial Markdown animation before editing")
        before = self.mobject._stamp()
        initial = self.mobject.copy()
        target = self.mobject.copy()
        target.set_source(self._source_target)
        if self.mobject._stamp() != before:
            raise RuntimeError("Markdown changed during animation preparation")
        if len(target.block_ranges) != len(initial.block_ranges):
            raise ValueError("Markdown morph requires equal block counts; use set_source for structural changes")
        self._source_initial, self.target_mobject = initial, target
        self._source_owned = True
        try:
            return super().begin()
        except BaseException as error:
            self._recover(error)
            raise

    def interpolate_mobject(self, alpha):
        self._source_alphas = []
        return super().interpolate_mobject(alpha)

    def interpolate_submobject(self, submobject, start, target, alpha):
        self._source_alphas.append(float(alpha))
        return super().interpolate_submobject(submobject, start, target, alpha)

    def finish(self):
        with _EDITS.hold(self.mobject, message="Markdown endpoint publication cannot reenter its document"):
            return self._finish_source()

    def _finish_source(self):
        try:
            super().finish()
            if self._source_alphas and all(a >= 1 for a in self._source_alphas):
                self.mobject._publish(self.target_mobject, positional=True)
            elif self._source_alphas and all(a <= 0 for a in self._source_alphas):
                self.mobject._publish(self._source_initial, positional=True)
            else:
                self.mobject._markdown_partial = True
        except BaseException as error:
            self._recover(error)
            raise
        finally:
            self._source_owned = False

    def _recover(self, error):
        try:
            self.abort()
        except BaseException as cleanup:
            _note(error, "Markdown cleanup also failed: " + type(cleanup).__name__)

    def abort(self):
        if not self._source_owned:
            return
        self._source_owned = False
        try:
            super().abort()
        finally:
            if self._source_initial is not None:
                self.mobject._publish(self._source_initial, positional=True)
