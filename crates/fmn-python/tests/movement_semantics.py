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


def check_phase_flow_replay_and_zero_time():
    from manimlib import PhaseFlow
    scene=Scene()
    line=Line([0,0,0],[1,0,0])
    original=line.get_points().copy()
    flow=PhaseFlow(lambda p:np.array([1.,0.,0.]),line,virtual_time=1.,run_time=.1)
    scene.play(flow)
    np.testing.assert_allclose(line.get_points(),original+[1.,0.,0.],atol=3e-6)
    scene.play(flow)
    np.testing.assert_allclose(line.get_points(),original+[2.,0.,0.],atol=3e-6)
    scene.play(PhaseFlow(lambda p:p,line,virtual_time=0.,run_time=.1))
    np.testing.assert_allclose(line.get_points(),original+[2.,0.,0.],atol=3e-6)


def check_flow_failure_recovery():
    from manimlib import PhaseFlow
    line=Line([1,0,0],[2,0,0])
    flow=PhaseFlow(lambda p:np.array([1.,0.,0.]),line,virtual_time=1.,suspend_mobject_updating=True)
    flow.begin()
    flow.interpolate(.25)
    before=line.get_points().copy()
    def fail(point):
        raise ValueError("flow failure")
    flow.function=fail
    try:
        flow.interpolate(.5)
    except ValueError:
        pass
    else:
        raise AssertionError("flow failure was ignored")
    assert not line._is_updating_suspended()
    flow.function=lambda p:np.array([1.,0.,0.])
    flow.begin()
    np.testing.assert_allclose(line.get_points(),before,atol=2e-6)
    flow.finish()
    np.testing.assert_allclose(line.get_points(),before+[1.,0.,0.],atol=2e-6)


def check_authored_path_sampler():
    from manimlib import MoveAlongPath
    calls=[]
    class Path(Line):
        def point_from_proportion(self,t):
            calls.append(t)
            return np.array([2*t,t*t,0.])
    path=Path([0,0,0],[2,1,0])
    moving=Line([-.1,0,0],[.1,0,0])
    seen=[]
    class Motion(MoveAlongPath):
        def interpolate(self,alpha):
            super().interpolate(alpha)
            seen.append((alpha,moving.get_center().copy()))
    Scene().play(Motion(moving,path,run_time=.2,rate_func=linear))
    assert calls and seen
    for alpha,center in seen:
        np.testing.assert_allclose(center,[2*alpha,alpha*alpha,0.],atol=3e-6)


def check_late_bound_path_and_move_hooks():
    from manimlib import MoveAlongPath
    import types
    path=Line([0,0,0],[2,0,0])
    moving=Line([-.1,0,0],[.1,0,0])
    animation=MoveAlongPath(moving,path,run_time=.1,rate_func=linear)
    moves=[]
    original_move=moving.move_to
    def sampler(self,t):
        return np.array([t,2*t,0.])
    def move(self,point):
        moves.append(np.array(point))
        return original_move(point)
    path.point_from_proportion=types.MethodType(sampler,path)
    moving.move_to=types.MethodType(move,moving)
    Scene().play(animation)
    assert moves
    np.testing.assert_allclose(moving.get_center(),[1.,2.,0.],atol=2e-6)


def check_path_remover_and_fractional_endpoint():
    from manimlib import MoveAlongPath
    scene=Scene()
    moving=Line([-.1,0,0],[.1,0,0])
    path=Line([-1,0,0],[1,0,0])
    scene.play(MoveAlongPath(moving,path,rate_func=linear,run_time=.1,
                            final_alpha_value=.25,remover=True))
    np.testing.assert_allclose(moving.get_center(),[-.5,0.,0.],atol=2e-6)
    assert moving not in scene.mobjects


def check_path_play_rate_has_no_redundant_probes():
    from manimlib import MoveAlongPath
    scene=Scene()
    moving=Line([-.1,0,0],[.1,0,0])
    path=Line([0,0,0],[2,0,0])
    samples=[]
    def rate(alpha):
        samples.append(alpha)
        return alpha*alpha
    scene.play(MoveAlongPath(moving,path),run_time=.5,rate_func=rate)
    assert samples
    assert all(any(abs(alpha-k/15)<1e-8 for k in range(16)) for alpha in samples), samples
    np.testing.assert_allclose(moving.get_center(),[2.,0.,0.],atol=2e-6)


def check_mixed_native_callback_succession():
    from manimlib import MoveAlongPath, Succession
    moving=Line([-.1,0,0],[.1,0,0])
    starts=[]
    class Authored(MoveAlongPath):
        def create_starting_mobject(self):
            starts.append(self.mobject.get_center().copy())
            return super().create_starting_mobject()
    from manimlib import _native
    scene=Scene()
    stock=MoveAlongPath(moving,Line([0,0,0],[1,0,0]),run_time=.1,rate_func=linear)
    authored=Authored(moving,Line([1,0,0],[2,1,0]),run_time=.1,rate_func=linear)
    assert not _native._requires_python_animation(stock)
    assert _native._requires_python_animation(authored)
    scene.play(Succession(stock,authored))
    assert len(starts)==1
    np.testing.assert_allclose(starts[0],[1.,0.,0.],atol=2e-6)
    np.testing.assert_allclose(moving.get_center(),[2.,1.,0.],atol=2e-6)


def check_path_failure_and_scene_reuse():
    from manimlib import MoveAlongPath
    error=ValueError("authored path failure")
    enabled=[False]
    class Path(Line):
        def point_from_proportion(self,t):
            if t>.2 and not enabled[0]:
                raise error
            return super().point_from_proportion(t)
    scene=Scene()
    moving=Line([-.1,0,0],[.1,0,0])
    path=Path([0,0,0],[1,0,0])
    motion=MoveAlongPath(moving,path,run_time=.2,rate_func=linear,suspend_mobject_updating=True)
    try:
        scene.play(AnimationGroup(motion))
    except ValueError as caught:
        assert caught is error
    else:
        raise AssertionError("authored path failure was ignored")
    assert not moving._is_updating_suspended()
    enabled[0]=True
    scene.play(motion)
    np.testing.assert_allclose(moving.get_center(),[1.,0.,0.],atol=2e-6)


def _read_y4m_luma(payload):
    header,offset=payload.split(b"\n",1)[0],payload.index(b"\n")+1
    if not header.startswith(b"YUV4MPEG2 "):
        raise ValueError("not a Y4M stream")
    fields={part[:1]:part[1:] for part in header.split()[1:]}
    width,height=int(fields[b"W"]),int(fields[b"H"])
    chroma=fields.get(b"C",b"420jpeg")
    if width<=0 or height<=0 or width%2 or height%2 or chroma not in (b"420",b"420jpeg",b"420mpeg2",b"420paldv"):
        raise ValueError("acceptance reader expects even-sized 8-bit 4:2:0")
    stride=width*height*3//2
    frames=[]
    while offset<len(payload):
        end=payload.index(b"\n",offset)
        if payload[offset:end].split()[0]!=b"FRAME":
            raise ValueError("missing frame marker")
        offset=end+1
        if len(payload)-offset<stride:
            raise ValueError("truncated frame")
        frames.append(np.frombuffer(payload[offset:offset+width*height],dtype=np.uint8).reshape(height,width).copy())
        offset+=stride
    return frames


def check_deformation_reaches_renderer():
    import tempfile
    from pathlib import Path
    from manimlib import Square
    from fmn_python import render_scene
    class Deformation(Scene):
        def construct(self):
            square=Square(side_length=1.,fill_opacity=1.,stroke_width=0).shift(np.array([-1.,0.,0.]))
            self.add(square)
            self.play(Homotopy(lambda x,y,z,t:(x+2*t,y,z),square,run_time=1.,rate_func=linear))
    directory=Path(tempfile.mkdtemp(prefix="fmn-movement-acceptance-"))
    result=render_scene(Deformation,directory/"deformation.y4m",resolution=(96,54),fps=4,threads=1)
    frames=_read_y4m_luma(result.destination.read_bytes())
    assert result.frame_count==len(frames)==4
    centers=[]
    for frame in frames:
        columns=np.nonzero(frame>128)[1]
        assert len(columns)>0, "the moving square was not rendered"
        centers.append(float(np.mean(columns+.5)))
    expected=[48.+(54./8.)*x for x in (-.5,0.,.5,1.)]
    np.testing.assert_allclose(centers,expected,atol=.8)
    print(f"movement renderer artifact retained: {result.destination}")


for _case in (check_homotopy_family_lag,check_start_hook_and_complex_map,
              check_time_span_and_reverse_interpolation,check_suspension_and_resume,
              check_nested_failure_and_recovery,check_scene_updater_failure_unwinds,
              check_phase_flow_replay_and_zero_time,check_flow_failure_recovery,
              check_authored_path_sampler,check_late_bound_path_and_move_hooks,
              check_path_remover_and_fractional_endpoint,check_path_play_rate_has_no_redundant_probes,
              check_mixed_native_callback_succession,check_path_failure_and_scene_reuse,
              check_deformation_reaches_renderer):
    _case()
