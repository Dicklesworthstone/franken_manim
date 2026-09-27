# Public update dispatch

`Mobject.update(dt, recurse=True)` traverses children through their public
`update(dt)` methods, child-first. An override accepting only `dt` is valid.
Its `super().update(...)` call chooses the base traversal's elapsed time and
recursion scope; omitting that call omits the base updater work for its subtree.
Shared children retain path-wise visits. Each node snapshots its child list and
then, at its own turn, its updater list, so edits have a defined visibility point.

Host callbacks complete before that invocation's native updater phase. Ordinary
families retain one native recursive crossing. When a child authors the public
protocol, the native phase uses self-only calls for the visited base operations,
with their chosen time values and suspension ancestry; it does not recursively
update every descendant again at each ancestor. No Stage borrow spans a Python
callback. An explicit update inside an updater is a separate invocation. A
host-only `_update_python_family` call does not start a native phase. Failures
preserve the original exception and discard pending native work, not authored
side effects. Invocation state is thread-local and is not copied or pickled
with a mobject.

## Public scene updates

`Scene.update_mobjects(dt)` updates the actual `scene.frame` first, followed by
the drawable roots captured at the start of the call. Camera callbacks can edit
scene membership, but those changes affect the next root snapshot. The frame
is not part of the drawable Stage and is not inserted into it for this purpose.
Camera suspension does not suspend unrelated drawable roots.

`Scene.should_update_mobjects()` checks the camera and the recursive public
`has_updaters()` query on each root; callbacks on deep descendants are not lost.
`always_update_mobjects` short-circuits that query. The existing public
`update_frame(dt)` retains its order: advance scene time, update objects, then
perform its normal capture/interaction policy, including its skip-mode policy.

These rules apply to direct public updates and animation-owned helper/snapshot
updates. They do **not** replace the separate native frame scheduler used by
`Scene.play` and `Scene.wait`. In particular, this change does not claim that
all drawable `update()` overrides are automatically invoked by that scheduler.

## Regression coverage

`crates/fmn-python/tests/public_update_dispatch.py` covers recursive overrides,
selected time/recursion, native-phase ordering, snapshots, suspension, exceptions,
reentry, copying/collection, animation helpers, camera-only update detection,
public scene time, and captured frames. Animation-helper Y4M and explicit public
frame PNG sequences are compared with independent controls at 1, 4 and 16
renderer workers. The installed runtime gate invokes this suite.
