# BN-26 — Arrow rebuilds keep a same-count NumPy view live

**Status:** Draft. Landed in W10 (fm-view-detach-on-become-p8c23); becomes Final
when the Python-portal gate passes.

## What changed

A NumPy view of a mobject's points (`p = mob.get_points()`) follows every edit
that keeps the record count, and detaches only when the record count changes.
Classic manim follows the same rule almost everywhere: `shift`, `rotate`,
`scale`, `Line.put_start_and_end_on` and an equal-size `become`/`restore` all
write in place.

Classic manim's `Arrow` is the exception. `Arrow.set_points_by_ends` rebuilds
the outline from scratch: `set_points` of a three-point stem, then a run of
`add_line_to` calls. So every Arrow rebuild reallocates the point array, even
when the rebuilt arrow has the same number of points. `Arrow.scale`, and so
`Arrow.set_height`, and `Arrow.put_start_and_end_on` all go through that
rebuild. A view taken before any of them is detached afterwards: it keeps
the old values, and writes through it move nothing.

FrankenManim rebuilds an Arrow in place when the point count is unchanged. The
view stays attached, reads the rebuilt points, and a write through it moves the
arrow, as it would for any other mobject. A rebuild that changes the count, such
as adding a `path_arc`, detaches the view in both engines.

## Migration guidance

- Code that writes through a view after rebuilding an Arrow now moves the arrow.
  If the old code relied on the write being lost, drop the write.
- To keep the pre-rebuild points, take a copy: `p = arrow.get_points().copy()`.
- To edit an Arrow's geometry, prefer its own API (`put_start_and_end_on`,
  `set_points_by_ends`) over writes through a stored view. Those calls behave
  the same in both engines.

## Evidence

- `crates/fmn-python/tests/native_arrow_lifecycle.py`
  `test_same_count_views_remain_live_and_resized_views_detach`: a same-count
  `set_points_by_ends` keeps the view live; a `path_arc` rebuild detaches it.
- The API fuzzer's seed-34 programs s34-000790 (`Arrow.scale(1.57,
  about_edge=DOWN)`, then a write through the earlier view) and s34-007103
  (`Arrow.set_height`). The Reference's write moves nothing; FrankenManim's
  moves the arrow by exactly the written offset.
- Probe on the pinned Reference (xvfb) and the portal:
  - the Reference detaches across `Arrow.scale`, `Arrow.set_height` and
    `Arrow.put_start_and_end_on`, and keeps the view live across
    `Line.put_start_and_end_on`, `Line.scale`, `Square.shift` and
    `Square.rotate`;
  - FrankenManim keeps all seven live.
- Pinned Reference:
  `scripts/manim_ref/manimlib/mobject/geometry.py::Arrow.set_points_by_ends`
  (lines 1123-1178) and `Arrow.scale` (line 1202), at commit
  `6199a00d4c1b1127ebe45cb629c3f22538b10e13`.
