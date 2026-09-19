"""Source-ordered text reveals over the native, identity-bearing glyph tree.

A glyph span can address any descendant, including a complete live numeric
family. Revealing it changes native child lists, never clones, flattens, or
re-typesets glyph geometry. Scribe's span order selects what is visible;
Marionette's original sibling order still controls compositing.
"""
from __future__ import annotations

import operator
from typing import Any


def _same_children(left, right):
    return len(left) == len(right) and all(a is b for a, b in zip(left, right))


def _note(error, cleanup):
    try:
        error.add_note("text reveal rollback also failed: " + type(cleanup).__name__)
    except BaseException:
        pass


class _GlyphRevealPlan:
    """Freeze paths and parent identities before hiding any part of a string."""

    def __init__(self, root):
        paths = []
        for raw in root._string_sub_paths:
            path = []
            for index in raw:
                if isinstance(index, bool):
                    raise ValueError("text span paths require non-negative integer indices")
                index = operator.index(index)
                if index < 0:
                    raise ValueError("text span paths require non-negative integer indices")
                path.append(index)
            paths.append(tuple(path))
        if not paths or len(paths) != len(root._string_sub_spans):
            raise ValueError("text reveal requires matching, nonempty source spans and family paths")
        ordered = sorted(paths)
        for left, right in zip(ordered, ordered[1:]):
            if right[:len(left)] == left:
                raise ValueError("text span paths must not duplicate or contain another glyph path")

        nodes, children, first, last = {(): root}, {}, {}, {}
        for ordinal, path in enumerate(paths):
            prefix, ancestors = (), {id(root)}
            for index in path:
                node = nodes[prefix]
                if prefix not in children:
                    children[prefix] = tuple(node.submobjects)
                if index >= len(children[prefix]):
                    raise ValueError("text span path no longer matches its native family")
                last[prefix] = ordinal + 1
                following = (*prefix, index)
                first.setdefault(following, ordinal + 1)
                node = children[prefix][index]
                if id(node) in ancestors:
                    raise ValueError("text reveal cannot traverse a cyclic glyph family")
                ancestors.add(id(node))
                nodes[following] = node
                prefix = following
        if paths == [()]:
            # A native whole-entry span may live in the root's own records.
            children[()] = tuple(root.submobjects)
            last[()] = 1

        self.root, self.count = root, len(paths)
        self._rows, self._data_visible = [], {}
        identities = set()
        for path in sorted(children, key=lambda item: (-len(item), item)):
            node, original = nodes[path], children[path]
            if id(node) in identities:
                raise ValueError("text reveal paths alias a mutable parent through multiple branches")
            identities.add(id(node))
            # Unmapped ornaments become visible with their completed parent,
            # not early with an unrelated glyph; restore every ornament at 1.
            thresholds = tuple(first.get((*path, i), last[path]) for i in range(len(original)))
            data = node.data.copy() if len(node.data) else None
            self._rows.append((node, original, thresholds, data, last[path]))
            self._data_visible[id(node)] = True

    def apply(self, count):
        """Apply a prefix transactionally, including backward timeline seeks."""
        count = min(max(operator.index(count), 0), self.count)
        operations, visible = [], {}
        for node, original, thresholds, data, completed in self._rows:
            desired = tuple(child for child, threshold in zip(original, thresholds) if count >= threshold)
            prior = tuple(node.submobjects)
            if data is not None:
                show = count >= completed
                visible[id(node)] = show
                if show != self._data_visible[id(node)]:
                    operations.append((node.set_data, node.data.copy(), data if show else data[:0]))
            if not _same_children(prior, desired):
                operations.append((node.set_submobjects, list(prior), list(desired)))
        attempted = []
        try:
            for setter, old, new in operations:
                # An authored setter can mutate the native family and THEN
                # raise. Include that setter in rollback, not just successes.
                attempted.append((setter, old))
                setter(new)
        except BaseException as error:
            for setter, old in reversed(attempted):
                try:
                    setter(old)
                except BaseException as cleanup:
                    _note(error, cleanup)
            raise
        self._data_visible.update(visible)

    def restore(self):
        self.apply(self.count)


def _apply_glyph_reveal(root, plan, count):
    if plan.root is not root:
        raise ValueError("text reveal plan belongs to a different StringMobject")
    plan.apply(count)


def install_text_reveal(native: Any) -> None:
    """Install on the shared bootstrap namespace without replacing classes."""
    g = vars(native)
    if g.get("_FMN_TEXT_REVEAL_INSTALLED", False):
        return
    g["_string_glyph_reveal_plan"] = _GlyphRevealPlan
    g["_apply_glyph_reveal"] = _apply_glyph_reveal
    g["_FMN_TEXT_REVEAL_INSTALLED"] = True
