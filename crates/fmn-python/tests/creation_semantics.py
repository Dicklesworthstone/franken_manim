"""Compiled-extension acceptance for live creation and reveal choreography."""
import math
import numpy as np
import manimlib as m


class NonlinearReveal(m.ShowPartial):
    def get_bounds(self, alpha):
        self.samples.append(float(alpha))
        return (0., alpha * alpha)
    def __init__(self, mob, **kwargs):
        self.samples = []
        super().__init__(mob, rate_func=m.linear, **kwargs)


scene = m.Scene()
curve = m.Line((0., 0., 0.), (4., 0., 0.))
reveal = NonlinearReveal(curve, run_time=1., final_alpha_value=.5)
scene.play(reveal)
assert .5 in reveal.samples, reveal.samples
assert np.allclose(curve.get_end(), [1., 0., 0.]), curve.get_end()
assert not curve.is_changing()

# A rule can agree with the obsolete four probes and disagree elsewhere.
class BetweenProbes(m.ShowPartial):
    def get_bounds(self, alpha):
        return (0., alpha + 8 * math.prod(alpha - p for p in (.125, .375, .625, .875)))

probe_scene = m.Scene()
probe_curve = m.Line((0., 0., 0.), (4., 0., 0.))
probe_animation = BetweenProbes(probe_curve, rate_func=m.linear, final_alpha_value=.5)
probe_scene.play(probe_animation)
assert np.allclose(probe_curve.get_end(), [4 * probe_animation.get_bounds(.5)[1], 0., 0.])
assert not np.isclose(probe_curve.get_end()[0], 2.)

# A stock class with a later instance override must not use its native kind.
patched_scene = m.Scene()
patched_curve = m.Line((0., 0., 0.), (4., 0., 0.))
patched = m.ShowCreation(patched_curve, rate_func=m.linear)
patched.get_bounds = lambda alpha: (.25, .75)
patched_scene.play(patched)
assert np.allclose(patched_curve.get_start(), [1., 0., 0.])
assert np.allclose(patched_curve.get_end(), [3., 0., 0.])

# Nested lagged/sequential groups still run on the native rational clock.
nested_scene = m.Scene()
a = m.Line((0., 0., 0.), (4., 0., 0.))
b = m.Line((0., 1., 0.), (4., 1., 0.))
first = NonlinearReveal(a, run_time=.5, final_alpha_value=.5)
second = NonlinearReveal(b, run_time=.5, final_alpha_value=.75)
nested_scene.play(m.Succession(m.AnimationGroup(first), second))
assert first.samples and second.samples
assert np.allclose(a.get_end(), [1., 0., 0.])
assert np.allclose(b.get_end(), [2.25, 1., 0.])

# The surface path must call Surface's UV partial operation, not a vector
# reveal against incompatible structured records.
surface_scene = m.Scene()
surface = m.ParametricSurface(lambda u, v: (u, v, 0.), resolution=(3, 3))
surface_start = surface.copy()
expected_surface = surface.copy()
expected_surface.pointwise_become_partial(surface_start, .2, .8)
surface_reveal = m.ShowCreation(surface, rate_func=m.linear)
surface_reveal.get_bounds = lambda alpha: (.2, .8)
surface_scene.play(surface_reveal)
assert surface.data.dtype == expected_surface.data.dtype
assert np.allclose(surface.get_points(), expected_surface.get_points())

# Retained passing flashes restore reusable full geometry. They also have
# the public ShowPartial hierarchy, including already-published imports.
from manimlib.animation.indication import ShowPassingFlash as QualifiedPassing
assert QualifiedPassing is m.ShowPassingFlash
assert issubclass(m.ShowPassingFlash, m.ShowPartial)
passing_scene = m.Scene()
passing_curve = m.Line((0., 0., 0.), (4., 0., 0.))
passing_points = passing_curve.get_points().copy()
passing_scene.play(m.ShowPassingFlash(passing_curve, remover=False))
assert np.array_equal(passing_curve.get_points(), passing_points)
assert passing_curve in passing_scene.mobjects

uncreate_scene = m.Scene()
uncreate_curve = m.Line((0., 0., 0.), (4., 0., 0.))
uncreate_scene.play(m.Uncreate(uncreate_curve, remover=False))
assert uncreate_curve in uncreate_scene.mobjects
assert np.allclose(uncreate_curve.get_start(), uncreate_curve.get_end())

# A failure after native playback has opened must not strand suspension or
# contaminate the next native play with a still-active reveal lifecycle.
class RevealFailure(RuntimeError):
    pass
class FailingReveal(m.ShowPartial):
    def get_bounds(self, alpha):
        if alpha > 0:
            raise RevealFailure("live-reveal-failure")
        return (0., 1.)

failure_scene = m.Scene()
failure_curve = m.Line((0., 0., 0.), (4., 0., 0.))
failed = FailingReveal(failure_curve, suspend_mobject_updating=True)
try:
    failure_scene.play(m.AnimationGroup(failed))
except RevealFailure as error:
    assert str(error) == "live-reveal-failure"
else:
    raise AssertionError("authored reveal callback was silently bypassed")
assert not failure_curve._is_updating_suspended()
assert not failure_curve.is_changing()
failed.abort()
failure_scene.play(failure_curve.animate.shift((1., 0., 0.)))
assert np.isfinite(failure_curve.get_points()).all()
