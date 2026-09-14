"""Real installed-wheel movement acceptance; never replaced by protocol doubles."""
import numpy as np
from manimlib import Animation, AnimationGroup, ComplexHomotopy, Homotopy, Line, Scene, VGroup, linear


def check_homotopy_family_lag():
    a,b=Line([0,0,0],[1,0,0]),Line([0,2,0],[1,2,0])
    initial_a,initial_b=a.get_points().copy(),b.get_points().copy()
    scene=Scene()
    observed=[]
    class Reveal(Homotopy):
        def interpolate(self,alpha):
            super().interpolate(alpha)
            if abs(alpha-.5)<1e-8:
                observed.append((a.get_points().copy(),b.get_points().copy()))
    scene.play(Reveal(lambda x,y,z,t:(x+t,y,z),VGroup(a,b),run_time=1,
                      rate_func=linear,lag_ratio=1))
    assert observed, "the native clock never sampled the middle frame"
    np.testing.assert_allclose(observed[0][0],initial_a+[.5,0,0],atol=2e-6)
    np.testing.assert_allclose(observed[0][1],initial_b,atol=2e-6)


def check_start_hook_and_complex_map():
    line=Line([1,2,3],[2,2,3])
    original=line.get_points().copy()
    calls=[]
    class Custom(ComplexHomotopy):
        def create_starting_mobject(self):
            calls.append(self)
            return super().create_starting_mobject()
    animation=Custom(lambda z,t:z*(1+t),line,rate_func=linear)
    animation.begin()
    animation.interpolate(.5)
    expected=original.copy()
    expected[:,:2]*=1.5
    np.testing.assert_allclose(line.get_points(),expected,atol=2e-6)
    assert calls==[animation]
    animation.finish()


def check_time_span_and_reverse_interpolation():
    line=Line([0,0,0],[1,0,0])
    original=line.get_points().copy()
    animation=Homotopy(lambda x,y,z,t:(x+t,y,z),line,run_time=2,
                       rate_func=linear,time_span=(.5,1.5),final_alpha_value=.5)
    animation.begin()
    for alpha,offset in ((.75,1.),(.25,0.),(.5,.5)):
        animation.interpolate(alpha)
        np.testing.assert_allclose(line.get_points(),original+[offset,0,0],atol=2e-6)
    animation.finish()
    np.testing.assert_allclose(line.get_points(),original+[.5,0,0],atol=2e-6)


def check_suspension_and_resume():
    a,b=Line([0,0,0],[1,0,0]),Line([0,2,0],[1,2,0])
    root=VGroup(a,b)
    b.suspend_updating()
    calls=[]
    a.add_updater(lambda mob,dt:calls.append(dt),call=False)
    animation=Homotopy(lambda x,y,z,t:(x+t,y,z),root,rate_func=linear,
                       suspend_mobject_updating=True)
    animation.begin()
    assert a._is_updating_suspended() and b._is_updating_suspended()
    animation.finish()
    assert not a._is_updating_suspended() and b._is_updating_suspended()
    assert calls==[0.]


def check_nested_failure_and_recovery():
    scene=Scene()
    line=Line([0,0,0],[1,0,0])
    error=ValueError("authored deformation failed")
    def deform(x,y,z,t):
        if t>.2:
            raise error
        return x+t,y,z
    animation=Homotopy(deform,line,suspend_mobject_updating=True,rate_func=linear)
    try:
        scene.play(AnimationGroup(animation))
    except ValueError as caught:
        assert caught is error
    else:
        raise AssertionError("the authored failure was swallowed")
    assert not line._is_updating_suspended()
    animation.homotopy=lambda x,y,z,t:(x,y+t,z)
    scene.play(animation,run_time=.1)
    assert not line._is_updating_suspended()


def check_scene_updater_failure_unwinds():
    scene=Scene()
    line=Line([0,0,0],[1,0,0])
    sibling=Line([0,2,0],[1,2,0])
    def fail(mob,dt):
        raise ValueError("scene updater")
    sibling.add_updater(fail,call=False)
    scene.add(sibling)
    try:
        scene.play(Homotopy(lambda x,y,z,t:(x+t,y,z),line,suspend_mobject_updating=True))
    except ValueError as error:
        assert str(error)=="scene updater"
    else:
        raise AssertionError("scene updater failure was swallowed")
    assert not line._is_updating_suspended()


for _case in (check_homotopy_family_lag,check_start_hook_and_complex_map,
              check_time_span_and_reverse_interpolation,check_suspension_and_resume,
              check_nested_failure_and_recovery,check_scene_updater_failure_unwinds):
    _case()
