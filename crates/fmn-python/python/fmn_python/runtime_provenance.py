"""Runtime content verification at the existing native publication boundary.

The one RenderSession remains the generation owner. This adapter snapshots
installed runtime inputs before native acquisition and verifies them after
source-provider callbacks and before publishing the provenance sidecar.
There is no second renderer, sink, clock, or configuration parser.
"""
from __future__ import annotations

from functools import wraps

from .runtime_identity import RuntimeIdentityError, capture_runtime


def _install_session_guard(rendering):
    Session = rendering.RenderSession
    if vars(Session).get("_fmn_runtime_provenance_installed", False):
        return
    original_enter, original_finish = Session.__enter__, Session.finish
    legacy_labels = rendering._runtime_identities

    @wraps(original_enter)
    def enter(self):
        self._check_owner()
        if (not self.reproducible or self._state != "new"
                or getattr(self.scene, "_fmn_owned_render_session", None) is not None):
            return original_enter(self)
        try:
            snapshot = capture_runtime(self._native)
            measured = snapshot.identities
            supplied = self.runtime_identities
            if supplied is not None and supplied != measured and supplied != legacy_labels(self._native):
                raise RuntimeIdentityError("supplied runtime identities disagree with the installed runtime")
        except RuntimeIdentityError as error:
            error_type = getattr(self._native, "_CapabilityError", RuntimeError)
            raise error_type("CAPABILITY: portal runtime input closure unavailable: " + str(error)) from error
        # The old console supplies version labels. Accept that spelling for
        # compatibility, but ALWAYS replace it with measured payload digests.
        # Arbitrary caller-supplied identifiers can no longer certify a build.
        self.runtime_identities = measured
        result = original_enter(self)
        self._fmn_runtime_snapshot = snapshot
        return result

    @wraps(original_finish)
    def finish(self):
        self._check_owner()
        if not self.reproducible or self._state != "active" or self._finishing:
            return original_finish(self)
        snapshot = getattr(self, "_fmn_runtime_snapshot", None)
        if snapshot is None:
            error = RuntimeIdentityError("certified generation has no acquired runtime input snapshot")
            self._cancel_preserving(error)
            raise error
        provider, publisher = self.sources, self._publish_manifest

        def checked_sources():
            values = provider() if callable(provider) else provider
            # Freeze the returned mapping too: custom Mapping iteration can
            # itself run Python and change runtime files. Check after it ends.
            values = rendering._source_snapshot(values)
            snapshot.verify()
            self.runtime_identities = snapshot.identities
            return values

        def checked_manifest(destination, format, resolution, fps, threads, seed,
                             artifact_report, sources, runtime_identities, cue_assets=None):
            # Native finish may capture a static final frame and invoke camera
            # hooks AFTER checked_sources. Detect edits there before sidecar
            # publication. RenderSession reports artifact_published=True on a
            # failure here; do not pretend the native artifact was rolled back.
            snapshot.verify()
            if runtime_identities != snapshot.identities:
                raise RuntimeIdentityError("runtime provenance was changed during native finalization")
            return publisher(destination, format, resolution, fps, threads, seed,
                             artifact_report, sources, snapshot.identities, cue_assets)

        self.sources, self._publish_manifest = checked_sources, checked_manifest
        try:
            return original_finish(self)
        finally:
            self.sources, self._publish_manifest = provider, publisher

    Session.__enter__, Session.finish = enter, finish
    Session._fmn_runtime_provenance_installed = True


def install_runtime_provenance(native):
    """Enable the same guarded session from the free functions, CLI and Scene."""
    if vars(native).get("_FMN_RUNTIME_PROVENANCE_INSTALLED", False):
        return
    from . import rendering

    _install_session_guard(rendering)
    Scene = native.Scene

    def scene_render_session(self, destination=None, *, format=None, resolution=None,
                             fps=None, threads=None, animation_range=None,
                             reproducible=False, sources=None, runtime_identities=None):
        destination, format = rendering._configured_destination(self, destination, format, native)
        return rendering.RenderSession(
            self, destination, format=format, resolution=resolution, fps=fps,
            threads=threads, animation_range=animation_range, reproducible=reproducible,
            sources=sources, runtime_identities=runtime_identities, _native=native,
        )

    def render(self, destination=None, *, format=None, resolution=None, fps=None,
               threads=None, animation_range=None, reproducible=False,
               sources=None, runtime_identities=None):
        session = scene_render_session(
            self, destination, format=format, resolution=resolution, fps=fps,
            threads=threads, animation_range=animation_range, reproducible=reproducible,
            sources=sources, runtime_identities=runtime_identities,
        )
        with session:
            try:
                self.run()
            except native.EndScene:
                pass
        if session.result is None:
            raise RuntimeError("scene execution ended without publishing its render generation")
        self.render_result = session.result
        return session.result

    for name, function in (("render", render), ("render_session", scene_render_session)):
        function.__name__ = name
        function.__qualname__ = Scene.__qualname__ + "." + name
        function.__module__ = Scene.__module__
        setattr(Scene, name, function)
    native._FMN_RUNTIME_PROVENANCE_INSTALLED = True
