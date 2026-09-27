"""Public child-update dispatch with one deferred native pass per invocation.

Python owns virtual calls and updater-list snapshots. Native updater slots are
run only after the host traversal returns, with no Stage borrow across Python.
Invocation state never enters an object's copied/pickled attribute dictionary.
This covers public updates and animation helpers, not Scene's native scheduler.
"""
from __future__ import annotations

from inspect import getattr_static
from threading import local
from types import MethodType


def install_updater_dispatch(native):
    g = vars(native)
    if g.get("_FMN_UPDATER_DISPATCH_INSTALLED", False):
        return
    Mobject = g["Mobject"]
    state = local()
    native_update = getattr_static(Mobject, "_update_native_mobject")

    class NativePass:
        def __init__(self):
            self.entries = []
            self.authored = False

    def python_family(self, dt, recurse):
        if self._is_updating_suspended():
            return
        frame = getattr(state, "frame", None)
        # A direct call to this private host-only helper must not accidentally
        # run native slots. Its children still use their public update hooks.
        jobs, ancestors = (NativePass(), ()) if frame is None or frame[0] is not self else frame[1:]
        state.frame = None
        try:
            host_visit(self, dt, recurse, jobs, ancestors)
        finally:
            state.frame = frame

    def host_visit(self, dt, recurse, jobs, ancestors):
        if recurse:
            for child in list(self.submobjects):
                previous = getattr(state, "delegate", None)
                state.delegate = (child, jobs, (self, ancestors))
                try:
                    # Only pass dt: valid authored overrides need not expose
                    # recurse. Their super() call chooses its own recursion.
                    callback = child.update
                    if (type(callback) is not MethodType or callback.__func__ is not update
                            or getattr_static(child, "_update_native_mobject") is not native_update):
                        jobs.authored = True
                    callback(dt)
                finally:
                    state.delegate = previous
        for updater in list(self.updaters):
            self._dispatch_updater(updater, dt)

    def update(self, dt=0, recurse=True):
        dt, recurse = float(dt), bool(recurse)
        delegate = getattr(state, "delegate", None)
        joined = delegate is not None and delegate[0] is self
        jobs, ancestors = (delegate[1], delegate[2]) if joined else (NativePass(), ())
        previous = getattr(state, "frame", None)
        # A deliberate update() inside an updater is an independent invocation,
        # not an implicit child visit, even when it updates this same object.
        state.delegate = None
        state.frame = (self, jobs, ancestors)
        try:
            self._update_python_family(dt, recurse)
            state.frame = None
            jobs.entries.append((self, dt, ancestors))
            if not joined and not jobs.authored:
                # Unchanged public protocols keep the existing single native
                # crossing and native family traversal, not one call per node.
                self._update_native_mobject(dt, recurse)
            elif not joined:
                # Child-first, path-wise (shared children keep their visits).
                # Self-only native calls avoid ticking descendants again at
                # every ancestor. An override omitting super opts out, and an
                # override using a different dt/recurse keeps those choices.
                for member, elapsed, parents in jobs.entries:
                    cursor = (member, parents)
                    while cursor and not cursor[0]._is_updating_suspended():
                        cursor = cursor[1]
                    if not cursor:
                        member._update_native_mobject(elapsed, False)
        finally:
            state.frame = previous
            state.delegate = delegate
            if not joined:
                jobs.entries.clear()
        return self

    for name, function in (("update", update), ("_update_python_family", python_family)):
        function.__name__ = name
        function.__qualname__ = Mobject.__qualname__ + "." + name
        function.__module__ = Mobject.__module__
        setattr(Mobject, name, function)
    Scene = g["Scene"]

    def update_mobjects(self, dt):
        dt = float(dt)
        # CameraFrame is intentionally absent from the drawable Stage. It is
        # nevertheless first in the Reference update order. Snapshot drawable
        # roots before its callbacks, as the native scene release path does.
        roots = list(self.mobjects)
        frame = self.frame
        frame.update(dt)
        for root in roots:
            if root is not frame:
                root.update(dt)

    def should_update_mobjects(self):
        if self.always_update_mobjects:
            return True
        # has_updaters owns the recursive/live family query. Looking only at
        # each root's list misses callbacks on descendants and the camera.
        return any(root.has_updaters() for root in (self.frame, *self.mobjects))

    for name, function in (("update_mobjects", update_mobjects),
                           ("should_update_mobjects", should_update_mobjects)):
        function.__name__ = name
        function.__qualname__ = Scene.__qualname__ + "." + name
        function.__module__ = Scene.__module__
        setattr(Scene, name, function)
    g["_FMN_UPDATER_DISPATCH_INSTALLED"] = True
