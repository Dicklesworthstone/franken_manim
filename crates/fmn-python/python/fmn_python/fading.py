"""Authored fading protocols over native Mobject operations and Choreo timing.

The grouped crossfade keeps two independently shaped families. It interpolates
copies of each family, never aligns one glyph outline onto an unrelated one.
"""
from __future__ import annotations

from functools import wraps
from operator import index
from typing import Any


def _method(cls, name, function):
    function.__name__ = name
    function.__qualname__ = cls.__qualname__ + "." + name
    function.__module__ = cls.__module__
    setattr(cls, name, function)


def install_fading(native: Any) -> None:
    """Install on the existing wheel classes, preserving qualified aliases."""
    g = vars(native)
    if g.get("_FMN_FADING_INSTALLED", False):
        return
    Animation, Transform = g["Animation"], g["Transform"]
    FadeTransform, Pieces = g["FadeTransform"], g["FadeTransformPieces"]
    Mobject, Group, VMobject = g["Mobject"], g["Group"], g["VMobject"]
    original_play = g["Scene"].play

    def fade_init(self, mobject, target_mobject, stretch=True, dim_to_match=1, **kwargs):
        if not isinstance(mobject, Mobject) or not isinstance(target_mobject, Mobject):
            raise TypeError("FadeTransform requires source and target Mobjects")
        dimension = index(dim_to_match)
        if dimension not in (0, 1, 2):
            raise ValueError("FadeTransform dim_to_match must be 0, 1, or 2")
        CameraFrame = g.get("CameraFrame")
        if CameraFrame is not None and any(
            isinstance(member, CameraFrame)
            for root in (mobject, target_mobject) for member in root.get_family()
        ):
            raise TypeError("FadeTransform crossfades drawable families, not camera frames")
        # A public save_state call is part of the pinned constructor protocol.
        # Retain the selected saved object: a later save_state must not change
        # which appearance this already-created transition restores.
        mobject.save_state()
        self._fade_source = mobject
        self._fade_saved_source = mobject.saved_state
        self.to_add_on_completion = target_mobject
        self.stretch, self.dim_to_match = bool(stretch), dimension
        self._fade_active = self._fade_finished = False
        self._fade_suspension = []
        self._fade_cleaned = False
        super(FadeTransform, self).__init__(Group(mobject, target_mobject.copy()), **kwargs)

    def release(self):
        prior, self._fade_suspension = self._fade_suspension, []
        first = None
        try:
            self.mobject.set_animating_status(False)
        except BaseException as error:
            first = error
        for member, was_suspended in prior:
            try:
                if not was_suspended and member._is_updating_suspended():
                    # Do not resume previously suspended descendants, and do
                    # not run an extra updater pass while unwinding a failure.
                    member.resume_updating(recurse=False, call_updater=False)
            except BaseException as error:
                if first is None:
                    first = error
        self.mobject_was_updating = False
        if first is not None:
            raise first

    def abort(self):
        if not self._fade_active:
            return
        self._fade_active = False
        release(self)

    def cancel_preserving(self):
        try:
            self.abort()
        except BaseException:
            pass

    def begin(self):
        if self._fade_active:
            self.abort()
        self._ensure_runtime_defaults()
        self.init_path_func()
        if self.time_span is not None:
            self.run_time = max(self.run_time, float(self.time_span[1]))
        self._fade_active, self._fade_finished, self._fade_cleaned = True, False, False
        self.mobject_was_updating = False
        try:
            self.mobject.set_animating_status(True)
            self.ending_mobject = self.mobject.copy()
            self.starting_mobject = self.create_starting_mobject()
            if not isinstance(self.starting_mobject, Mobject) or len(self.starting_mobject.submobjects) != 2:
                raise TypeError("FadeTransform starting mobject must contain source and target families")
            live_ids = {id(member) for member in self.mobject.get_family()}
            if any(id(member) in live_ids for member in self.starting_mobject.get_family()):
                raise ValueError("FadeTransform starting copy aliases its live animation family")
            start, end = self.starting_mobject, self.ending_mobject
            self.ghost_to(start[1], start[0])
            self.ghost_to(end[0], end[1])
            # Ordinary ghosting changes placement/style only. An authored
            # ghost may also refine its own geometry: reconcile each branch
            # across time, never source geometry against target geometry.
            start.align_data_and_family(end)
            self.mobject.align_data_and_family(start)
            self.mobject.align_data_and_family(end)
            if self.suspend_mobject_updating:
                seen = set()
                for member in self.mobject.get_family():
                    if id(member) not in seen:
                        seen.add(id(member))
                        self._fade_suspension.append((member, member._is_updating_suspended()))
                self.mobject_was_updating = not self.mobject._is_updating_suspended()
                self.mobject.suspend_updating()
            self.families = list(self.get_all_families_zipped())
            # Endpoints are ready before the sole alpha-zero interpolation;
            # nonzero rate(0) and authored hooks see the actual fade state.
            self.interpolate(0.0)
        except BaseException:
            cancel_preserving(self)
            raise

    def all_mobjects(self):
        return self.mobject, self.starting_mobject, self.ending_mobject

    def families_zipped(self):
        return Animation.get_all_families_zipped(self)

    def interpolate_submobject(self, current, start, end, alpha):
        current.interpolate(start, end, alpha, self.path_func)
        return self

    def finish(self):
        if not self._fade_active:
            if self._fade_finished:
                return
            raise RuntimeError("FadeTransform must begin before finish")
        try:
            self.interpolate(self.final_alpha_value)
            release(self)
        except BaseException:
            cancel_preserving(self)
            raise
        self._fade_active, self._fade_finished = False, True

    def cleanup(self, scene):
        # Succession cleans all children, including ones outside a shortened
        # final-alpha range. An unbegun child must not install its target.
        if not self._fade_finished or self._fade_cleaned:
            return
        scene.remove(self.mobject, self._fade_source)
        self._fade_source.become(self._fade_saved_source)
        if not self.is_remover():
            scene.add(self.to_add_on_completion)
        self._fade_cleaned = True

    def pieces_init(self, mobject, target_mobject, **kwargs):
        if not isinstance(mobject, VMobject) or not isinstance(target_mobject, VMobject):
            raise TypeError("FadeTransformPieces requires VMobject families")
        if not mobject.family_members_with_points() or not target_mobject.family_members_with_points():
            raise ValueError("FadeTransformPieces requires point-bearing families")
        super(Pieces, self).__init__(mobject, target_mobject, **kwargs)

    def pieces_begin(self):
        # Family correspondence is required for every piece to ghost. Point
        # alignment between unlike source/target glyphs is neither needed nor
        # desirable; each piece fades independently with its own topology.
        self.mobject[0].align_family(self.mobject[1])
        super(Pieces, self).begin()

    # Retain all published identities; their inherited Transform interpolation
    # now runs rather than reducing the object to a native endpoint-only spec.
    FadeTransform.__bases__ = (Transform,)
    FadeTransform._native_kind = Pieces._native_kind = None
    FadeTransform.replace_mobject_with_target_in_scene = False
    for name, function in {
        "__init__": fade_init, "begin": begin, "finish": finish,
        "get_all_mobjects": all_mobjects, "get_all_families_zipped": families_zipped,
        "interpolate_submobject": interpolate_submobject,
        "clean_up_from_scene": cleanup, "abort": abort,
    }.items():
        _method(FadeTransform, name, function)
    _method(Pieces, "__init__", pieces_init)
    _method(Pieces, "begin", pieces_begin)

    @wraps(original_play)
    def play(self, *animations, **kwargs):
        fades, seen, visiting = [], set(), set()
        def visit(animation):
            if id(animation) in visiting:
                raise ValueError("Animation composition contains a cycle")
            if id(animation) in seen:
                return
            seen.add(id(animation))
            visiting.add(id(animation))
            if isinstance(animation, FadeTransform):
                fades.append(animation)
            if isinstance(animation, g["AnimationGroup"]):
                for child in animation.animations:
                    visit(child)
            visiting.remove(id(animation))
        for animation in animations:
            visit(animation)
        try:
            return original_play(self, *animations, **kwargs)
        except BaseException:
            for animation in reversed(fades):
                cancel_preserving(animation)
            raise

    g["Scene"].play = play
    g["_FMN_FADING_INSTALLED"] = True
