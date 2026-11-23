//! Portal playback policy over Choreo's clock and Reel's output timeline.
//!
//! Skipping changes capture and endpoint work, never invents a second clock.
//! Audio cue timestamps remain scene-relative in Proscenium; only the output
//! adapter removes skipped intervals when submitting cues to the native mixer.

use super::*;

/// Allocate the authored tracker's record plane and typed f64 state together.
/// Python runs the normal virtual hooks only after this allocation succeeds.
/// Never replace a nursery which those hooks (or a schema descriptor) already
/// initialized: that would discard its records, views and family identities.
#[pyfunction]
pub(crate) fn _portal_allocate_tracker(
    target: &Bound<'_, BridgeMobject>,
    kind: u8,
) -> PyResult<()> {
    if kind > 2 {
        return Err(PyValueError::new_err("unknown native tracker encoding"));
    }
    let check = |cell: &BridgeMobject| -> PyResult<()> {
        if cell.initialized || cell.engine.is_some() || cell.mob.is_some() {
            return Err(PyRuntimeError::new_err(
                "tracker engine initialization may run only once, before scene entry",
            ));
        }
        Ok(())
    };
    check(&*target.try_borrow()?)?;
    let schema = parse_schema(target)?;
    let buffer = RecordBuffer::new(schema, 0).map_err(record_error_to_py)?;
    // Reuse Marionette's encodings, including its deterministic logarithm.
    // The public init_uniforms hook subsequently installs the authored value.
    let mut nursery = Nursery::value_tracker(kind, if kind == 1 { 1.0 } else { 0.0 }, 0.0);
    nursery
        .stage
        .get_mut(nursery.root)
        .ok_or_else(|| StaleHandleError::new_err("new tracker root is stale"))?
        .buffer = buffer;
    // parse_schema may execute Python. Recheck under the publication borrow;
    // neither a failed schema nor reentrant initialization mutates its target.
    let mut cell = target.try_borrow_mut()?;
    check(&cell)?;
    cell.nursery = Some(nursery);
    cell.initialized = true;
    Ok(())
}

/// Determine whether native slots alone would bypass a public scene/object
/// update protocol. Python performs descriptor-safe inspection of the completed
/// runtime, without executing authored update methods during admission.
pub(super) fn requires_public_scene_update(scene: &Bound<'_, PyScene>) -> PyResult<bool> {
    scene
        .call_method0("_fmn_requires_public_scene_update")?
        .extract()
}

/// Run the released scene-updater phase. A true second component means the
/// host has completed BOTH Python callbacks and native slots, so Choreo must
/// resume at event delivery/capture instead of repeating the native pass.
pub(super) fn run_scene_update_phase(scene: &Bound<'_, PyScene>, dt: f64) -> PyResult<(u64, bool)> {
    let delta = dt.into_pyobject(scene.py())?.into_any();
    run_scene_update_phase_with_delta(scene, &delta)
}

fn run_scene_update_phase_with_delta(
    scene: &Bound<'_, PyScene>,
    delta: &Bound<'_, PyAny>,
) -> PyResult<(u64, bool)> {
    let start = Instant::now();
    let completed = scene
        .call_method1("_fmn_dispatch_public_scene_update", (delta,))?
        .extract::<bool>()?;
    if completed {
        Ok((
            u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX),
            true,
        ))
    } else {
        run_python_updaters_with_delta(scene, delta).map(|elapsed| (elapsed, false))
    }
}

/// Scope the authoritative one-frame delta around a released updater phase.
/// No Scene borrow crosses a Python callback. Nested phases restore their
/// caller's token, including when an authored callback raises an exception.
fn with_frame_step<T>(
    scene: &Bound<'_, PyScene>,
    dt: f64,
    fps: u32,
    run: impl FnOnce(&Bound<'_, PyAny>) -> PyResult<T>,
) -> PyResult<T> {
    let delta = dt.into_pyobject(scene.py())?.into_any();
    let previous = scene
        .borrow_mut()
        .frame_step_context
        .replace(FrameStepContext {
            delta: delta.clone().unbind(),
            frames: 1,
            fps,
        });
    let result = run(&delta);
    scene.borrow_mut().frame_step_context = previous;
    result
}

/// Complete native slots before an authored capture observer reads their
/// results. Stock capture state can still be recovered from the frozen packet,
/// so its ordinary native driver keeps ownership of that phase.
pub(super) fn run_scene_capture_frame_update_phase(
    scene: &Bound<'_, PyScene>,
    dt: f64,
    fps: u32,
) -> PyResult<(u64, u64, bool)> {
    let (python_ns, mut completed) = with_frame_step(scene, dt, fps, |delta| {
        run_scene_update_phase_with_delta(scene, delta)
    })?;
    let engine = Rc::clone(&scene.borrow().engine);
    let check_failure = || -> PyResult<()> {
        if let Some(error) = engine.borrow().stage().updater_error().cloned() {
            return Err(scene_error(fmn_anim::AnimError::from(error).into()));
        }
        Ok(())
    };
    check_failure()?;
    let mut native_ns = 0;
    // Reinspect after Python updaters: one may have installed a capture
    // descriptor in this very frame. Admission itself invokes no descriptor.
    if !completed
        && scene
            .py()
            .import("manimlib")?
            .getattr("_fmn_readback_needs_host_capture")?
            .call1((scene,))?
            .extract::<bool>()?
    {
        let started = Instant::now();
        {
            let mut runtime = engine.borrow_mut();
            let time = runtime.time().to_f64();
            // This is precisely complete_play/wait_frame's native phase:
            // the clock already advanced, and suspension/root policy remains
            // Marionette's. The cursor completion marker prevents a repeat.
            runtime.stage_mut().update_at_time(dt, time);
        }
        native_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        check_failure()?;
        completed = true;
    }
    Ok((python_ns, native_ns, completed))
}

pub(super) fn run_python_frame_updaters(
    scene: &Bound<'_, PyScene>,
    dt: f64,
    fps: u32,
) -> PyResult<u64> {
    with_frame_step(scene, dt, fps, |delta| {
        run_python_updaters_with_delta(scene, delta)
    })
}

pub(super) fn run_animation_frame_phase(
    scene: &Bound<'_, PyScene>,
    animation: &Bound<'_, PyAny>,
    boundary: &fmn_anim::AnimationBoundary,
) -> PyResult<()> {
    let fps = boundary.sample.time.fps();
    with_frame_step(scene, boundary.dt, fps, |delta| {
        if animation.hasattr("update_mobjects")? {
            animation.call_method1("update_mobjects", (delta,))?;
        }
        // Camera choreography defers each child's helper update until its
        // interpolation turn. Keep this same delta token alive through the
        // whole ordered pair so those updates retain their native provenance.
        if animation.hasattr("_fmn_interpolate_at_frame")? {
            animation.call_method1(
                "_fmn_interpolate_at_frame",
                (boundary.sample.frame, fps, boundary.alpha),
            )?;
        } else {
            animation.call_method1("interpolate", (boundary.alpha,))?;
        }
        Ok(())
    })
}

#[derive(Default)]
pub(super) struct OutputTimeline {
    pub(super) final_state_only: bool,
    origin_frame: i64,
    last_frame: i64,
    skipping: bool,
    omitted: Vec<(i64, i64)>,
}

impl OutputTimeline {
    /// Start an insert at the live clock, without including pre-recording time.
    pub(super) fn start_at(&mut self, frame: i64) -> PyResult<()> {
        if frame < 0 || self.last_frame != 0 || !self.omitted.is_empty() {
            return Err(PyRuntimeError::new_err(
                "recording timeline requires an unused timeline and a nonnegative frame",
            ));
        }
        self.origin_frame = frame;
        self.last_frame = frame;
        Ok(())
    }

    pub(super) fn new(final_state_only: bool) -> Self {
        Self {
            final_state_only,
            ..Self::default()
        }
    }

    pub(super) fn observe(&mut self, frame: i64, skipping: bool) -> PyResult<()> {
        if frame < self.last_frame {
            return Err(PyRuntimeError::new_err(
                "portal-render: cannot rewind the clock of an active output generation",
            ));
        }
        if self.skipping && frame > self.last_frame {
            if let Some(last) = self.omitted.last_mut()
                && last.1 == self.last_frame
            {
                last.1 = frame;
            } else {
                if self.omitted.len() >= PORTAL_MAX_RENDER_FRAMES as usize {
                    return Err(PyRuntimeError::new_err(
                        "portal-render: skipped-interval budget exhausted",
                    ));
                }
                self.omitted.push((self.last_frame, frame));
            }
        }
        self.last_frame = frame;
        self.skipping = skipping;
        Ok(())
    }

    /// Absolute scene interval for one selected per-call audio clip. A
    /// mid-call cut is not a contiguous interval and must never be flattened
    /// silently into different audio. Ordinary subdivision selects at pre_play.
    pub(super) fn scene_window(&self) -> PyResult<(i64, i64)> {
        if !self.omitted.is_empty() {
            return Err(PyRuntimeError::new_err(
                "audio subdivision cannot contain skipped intervals inside one clip",
            ));
        }
        Ok((self.origin_frame, self.last_frame))
    }

    pub(super) fn output_frame(&self, frame: i64) -> PyResult<i64> {
        if frame < self.origin_frame || frame > self.last_frame {
            return Err(PyRuntimeError::new_err(
                "portal-render: audio timestamp is outside the observed scene timeline",
            ));
        }
        let removed: i64 = self
            .omitted
            .iter()
            .map(|&(start, end)| (frame.min(end) - start).max(0))
            .sum();
        Ok(frame - self.origin_frame - removed)
    }

    /// Absolute writer timestamps are sample-grid positions, independent of
    /// the call-site clock. Preserve pre-origin overlap as a negative prefix
    /// for Reel to clip, and remove exactly the omitted sample intervals.
    pub(super) fn output_sample(&self, sample: i64, fps: u32, sample_rate: u32) -> PyResult<i64> {
        let origin = fmn_output::frames_to_samples(self.origin_frame, fps, sample_rate)
            .map_err(native_error)?;
        let mut removed = 0_i128;
        for &(start, end) in &self.omitted {
            let start =
                fmn_output::frames_to_samples(start, fps, sample_rate).map_err(native_error)?;
            let end = fmn_output::frames_to_samples(end, fps, sample_rate).map_err(native_error)?;
            removed += (i128::from(sample.min(end)) - i128::from(start)).max(0);
        }
        i64::try_from(i128::from(sample) - i128::from(origin) - removed)
            .map_err(|_| PyValueError::new_err("absolute audio timestamp exceeds the sample grid"))
    }
}

impl PortalRenderSession {
    fn output_timeline(&self) -> &OutputTimeline {
        match self {
            Self::Frames(session) => &session.timeline,
            Self::Preview(session) => &session.timeline,
            Self::Vector(session) => &session.timeline,
            Self::Soundtrack { timeline, .. } => timeline,
        }
    }

    pub(super) fn output_timeline_mut(&mut self) -> &mut OutputTimeline {
        match self {
            Self::Frames(session) => &mut session.timeline,
            Self::Preview(session) => &mut session.timeline,
            Self::Vector(session) => &mut session.timeline,
            Self::Soundtrack { timeline, .. } => timeline,
        }
    }
}

pub(super) fn requested_skip(scene: &Bound<'_, PyScene>) -> PyResult<bool> {
    let requested = match scene.getattr("skip_animations") {
        Ok(value) => value.is_truthy()?,
        Err(error) if error.is_instance_of::<pyo3::exceptions::PyAttributeError>(scene.py()) => {
            false
        }
        Err(error) => return Err(error),
    };
    let slot = Arc::clone(&scene.borrow().render);
    let slot = slot
        .lock()
        .map_err(|_| PyRuntimeError::new_err("portal render session lock was poisoned"))?;
    Ok(requested
        || slot
            .as_ref()
            .is_some_and(|session| session.output_timeline().final_state_only))
}

/// Called at segment boundaries, after Python pre_play/range hooks and before
/// any animation begins. No Scene borrow survives Python attribute dispatch.
pub(super) fn synchronize(scene: &Bound<'_, PyScene>) -> PyResult<()> {
    let engine = Rc::clone(&scene.borrow().engine);
    let generation = Arc::clone(&scene.borrow().render_generation);
    let skipping = requested_skip(scene)?;
    // The public skip flag can be an authored descriptor. Its callback must
    // not redirect this boundary into a newly installed output generation.
    if !Rc::ptr_eq(&engine, &scene.borrow().engine)
        || !Arc::ptr_eq(&generation, &scene.borrow().render_generation)
    {
        return Err(PyRuntimeError::new_err(
            "portal render ownership changed during playback synchronization",
        ));
    }
    let frame = engine.borrow().time().frames();
    let slot = Arc::clone(&scene.borrow().render);
    if let Some(session) = slot
        .lock()
        .map_err(|_| PyRuntimeError::new_err("portal render session lock was poisoned"))?
        .as_mut()
    {
        session.output_timeline_mut().observe(frame, skipping)?;
    }
    engine.borrow_mut().set_skipping(skipping);
    Ok(())
}


const PRESENTER_ACTIVE: &str = "_fmn_presenter_hold_active";

pub(super) fn presenter_active(scene: &Bound<'_, PyScene>) -> PyResult<bool> {
    Ok(scene
        .getattr("__dict__")?
        .cast::<PyDict>()?
        .get_item(PRESENTER_ACTIVE)?
        .is_some())
}

fn presenter_decision(scene: &Bound<'_, PyScene>) -> PyResult<fmn_scene::HoldDecision> {
    scene.py().check_signals()?;
    if scene.call_method0("is_window_closing")?.is_truthy()? {
        return Ok(fmn_scene::HoldDecision::EndScene);
    }
    Ok(if requested_skip(scene)? || !scene.getattr("hold_on_wait")?.is_truthy()? {
        fmn_scene::HoldDecision::Release
    } else {
        fmn_scene::HoldDecision::Continue
    })
}

/// Drive the existing presenter/idle capture seam with host callbacks released
/// from every native borrow. The host owns event pumping and optional pacing;
/// Proscenium alone owns frame sampling, updaters, capture and play counts.
#[pyfunction]
#[pyo3(signature = (scene, wait = false))]
pub(crate) fn _portal_presenter_hold(scene: &Bound<'_, PyScene>, wait: bool) -> PyResult<()> {
    if presenter_active(scene)? {
        return Err(PyRuntimeError::new_err(
            "cannot reenter this Scene's presenter hold",
        ));
    }
    let namespace = scene.getattr("__dict__")?;
    let namespace = namespace.cast::<PyDict>()?;
    let inside_frame = scene.borrow().frame_step_context.is_some();
    let executing = namespace
        .get_item("_fmn_scene_execution")?
        .is_some_and(|owner| !owner.is_none());
    let initial_allowed = namespace
        .get_item("_fmn_presenter_initial_allowed")?
        .map(|value| value.extract::<bool>())
        .transpose()?
        .unwrap_or(false);
    if inside_frame || (!wait && executing && !initial_allowed) {
        return Err(PyRuntimeError::new_err(
            "cannot start a presenter hold during an active play/wait",
        ));
    }
    // Hold identity as well as references: an authored poll must not redirect
    // a released frame into a replacement scene/output generation.
    let engine = Rc::clone(&scene.borrow().engine);
    let generation = Arc::clone(&scene.borrow().render_generation);
    let check_owner = || -> PyResult<()> {
        if !Rc::ptr_eq(&engine, &scene.borrow().engine)
            || !Arc::ptr_eq(&generation, &scene.borrow().render_generation)
        {
            return Err(PyRuntimeError::new_err(
                "presenter scene/output ownership changed during a host callback",
            ));
        }
        Ok(())
    };
    namespace.set_item(PRESENTER_ACTIVE, true)?;
    let result = (|| {
        synchronize(scene)?;
        check_owner()?;
        if !wait && engine.borrow().is_skipping() {
            return Ok(());
        }
        let declaration = namespace.get_item("_fmn_presenter_controller")?.ok_or_else(|| {
            CapabilityError::new_err(
                "presenter holds require Scene.set_presenter_controller(poll, max_frames=...); \
                 use ignore_presenter_mode=True or wait_frames for a finite headless wait",
            )
        })?;
        let (controller, max_frames): (Py<PyAny>, u64) = declaration.extract()?;
        if !controller.bind(scene.py()).is_callable() || !(1..=1_048_576).contains(&max_frames) {
            return Err(PyValueError::new_err(
                "presenter controller must be callable with a frame budget in 1..=1048576",
            ));
        }
        if let Some(owner) = namespace.get_item("_fmn_owned_render_session")?
            && owner.getattr("reproducible")?.is_truthy()?
        {
            return Err(CapabilityError::new_err(
                "host-controlled presenter input is not a certified render closure; \
                 use a standard live session or finite authored waits",
            ));
        }
        prepare_portal_readback(scene)?;
        let initial_completed = if wait {
            run_scene_update_phase(scene, 0.0)?.1
        } else {
            false
        };
        check_owner()?;
        let mut sink = PortalSceneSink {
            render: Arc::clone(&scene.borrow().render),
            readback: Rc::clone(&scene.borrow().readback),
            readback_every_frame: true,
            ..PortalSceneSink::default()
        };
        let kind = if wait {
            fmn_scene::HoldKind::Wait
        } else {
            fmn_scene::HoldKind::Initial
        };
        let mut hold = engine
            .borrow_mut()
            .begin_stepped_hold(kind, max_frames, initial_completed, &mut sink)
            .map_err(scene_error)?;
        let driven: PyResult<()> = (|| {
            loop {
                engine
                    .borrow_mut()
                    .prepare_stepped_hold_poll(&hold)
                    .map_err(scene_error)?;
                let mut decision = presenter_decision(scene)?;
                check_owner()?;
                if decision == fmn_scene::HoldDecision::Continue {
                    crossing::record(CrossingClass::Other);
                    let response = controller
                        .bind(scene.py())
                        .call1((scene, if wait { "wait" } else { "initial" }))?;
                    if !response.is_none()
                        && !response.is_instance_of::<pyo3::types::PyBool>()
                    {
                        return Err(PyTypeError::new_err(
                            "presenter poll must return True, False, or None",
                        ));
                    }
                    decision = presenter_decision(scene)?;
                    check_owner()?;
                    if decision == fmn_scene::HoldDecision::Continue
                        && !response.is_none()
                        && !response.is_truthy()?
                    {
                        decision = fmn_scene::HoldDecision::Release;
                    }
                }
                match decision {
                    fmn_scene::HoldDecision::Release => break,
                    fmn_scene::HoldDecision::EndScene => {
                        return Err(EndScene::new_err("presenter window closing"));
                    }
                    fmn_scene::HoldDecision::Continue => {}
                }
                let mut frame = engine
                    .borrow_mut()
                    .prepare_stepped_hold_frame(&mut hold, &mut sink)
                    .map_err(scene_error)?;
                let (python_ns, native_updater_ns, completed) =
                    run_scene_capture_frame_update_phase(scene, frame.dt(), frame.time().fps())?;
                check_owner()?;
                if completed {
                    frame.mark_scene_updaters_complete();
                }
                synchronize_portal_camera(scene)?;
                check_owner()?;
                let started = Instant::now();
                engine
                    .borrow_mut()
                    .complete_stepped_hold_frame(&mut hold, frame, &mut sink)
                    .map_err(scene_error)?;
                crossing::record_phase(
                    python_ns,
                    portal_elapsed_ns(started).saturating_add(native_updater_ns),
                );
            }
            synchronize(scene)?;
            check_owner()?;
            Ok(())
        })();
        if let Err(error) = driven {
            engine.borrow_mut().abort_stepped_hold(hold, &mut sink);
            return Err(error);
        }
        engine
            .borrow_mut()
            .finish_stepped_hold(hold, &mut sink)
            .map_err(scene_error)
    })();
    // Cleanup cannot replace an authored exception or its traceback. Work
    // already performed is not rolled back or repeated after a failed poll.
    let released = namespace.del_item(PRESENTER_ACTIVE);
    match result {
        Ok(()) => {
            released?;
            scene.setattr("hold_on_wait", true)
        }
        Err(error) => Err(error),
    }
}

/// Host presenter waits share the ordinary native frame, readback and writer
/// paths. Arbitrary host input is outside the certified render closure.
#[cfg(any(test, feature = "gauntlet"))]
pub fn run_portal_gauntlet_presenter() -> Result<u64, String> {
    crate::with_python_test_module("host presenter holds", |py, _module, globals| {
        let code = CString::new(include_str!("../tests/native_presenter.py"))
            .map_err(|error| error.to_string())?;
        py.run(code.as_c_str(), Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        globals
            .get_item("run_native_presenter")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "presenter acceptance entry point is absent".to_owned())?
            .call0()
            .inspect_err(|error| error.print(py))
            .and_then(|value| value.extract::<u64>())
            .map_err(|error| error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_presenter_holds_use_native_frames() {
        assert_eq!(run_portal_gauntlet_presenter().unwrap(), 8);
    }

    #[test]
    fn absolute_audio_positions_keep_pre_origin_overlap_and_exact_skipped_endpoints() {
        let mut timeline = OutputTimeline::default();
        timeline.start_at(3).unwrap();
        timeline.observe(4, true).unwrap();
        timeline.observe(6, false).unwrap();
        let origin = fmn_output::frames_to_samples(3, 29, 48_000).unwrap();
        let cut_start = fmn_output::frames_to_samples(4, 29, 48_000).unwrap();
        let cut_end = fmn_output::frames_to_samples(6, 29, 48_000).unwrap();
        assert_eq!(timeline.output_sample(origin - 3, 29, 48_000).unwrap(), -3);
        assert_eq!(
            timeline.output_sample(cut_start + 7, 29, 48_000).unwrap(),
            cut_start - origin
        );
        assert_eq!(
            timeline.output_sample(cut_end + 11, 29, 48_000).unwrap(),
            cut_start - origin + 11
        );
        assert!(timeline.output_sample(i64::MIN, 29, 48_000).is_err());
    }

    #[test]
    fn frame_authored_camera_progress_uses_local_integer_time() {
        let start = 1_i64 << 60;
        assert_eq!(start as f64, (start + 1) as f64);
        assert_eq!(camera_frame_alpha(start, start, 43), 0.0);
        assert_eq!(camera_frame_alpha(start + 21, start, 43), 21.0 / 43.0);
        assert!(camera_frame_alpha(start + 42, start, 43) < 1.0);
        assert_eq!(camera_frame_alpha(start + 43, start, 43), 1.0);
        assert!(camera_frame_alpha(i64::MAX - 1, 0, i64::MAX as u64) < 1.0);
        assert_eq!(camera_frame_alpha(i64::MAX, 0, i64::MAX as u64), 1.0);
        assert_eq!(camera_frame_alpha(start, start, 0), 1.0);
    }

    #[test]
    fn public_scene_update_dispatch() {
        crate::with_python_test_module("native scene update dispatch", |py, _module, globals| {
            let source =
                CString::new(include_str!("../tests/native_scene_update_dispatch.py")).unwrap();
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .unwrap();
            globals
                .get_item("run_native_scene_update_dispatch")
                .unwrap()
                .unwrap()
                .call0()
                .inspect_err(|error| error.print(py))
                .unwrap();
        });
    }

    #[test]
    fn authored_tracker_lifecycle_and_native_rendering() {
        crate::with_python_test_module("tracker lifecycle", |py, _module, globals| {
            globals
                .set_item(
                    "__file__",
                    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/tracker_lifecycle.py"),
                )
                .unwrap();
            let source = CString::new(include_str!("../tests/tracker_lifecycle.py")).unwrap();
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .unwrap();
            globals
                .get_item("run_tracker_lifecycle")
                .unwrap()
                .unwrap()
                .call0()
                .inspect_err(|error| error.print(py))
                .unwrap();
        });
    }

    #[test]
    fn insert_timeline_rebases_live_time_and_skipped_intervals() {
        let mut timeline = OutputTimeline::default();
        timeline.start_at(90).unwrap();
        timeline.observe(90, false).unwrap();
        timeline.observe(96, true).unwrap();
        timeline.observe(102, false).unwrap();
        timeline.observe(108, false).unwrap();
        for (scene, output) in [(90, 0), (96, 6), (99, 6), (102, 6), (108, 12)] {
            assert_eq!(timeline.output_frame(scene).unwrap(), output);
        }
        assert!(timeline.output_frame(89).is_err());
        assert!(timeline.output_frame(109).is_err());
        assert!(timeline.observe(107, false).is_err());
        assert!(timeline.start_at(0).is_err());
    }

    #[test]
    fn omitted_intervals_rebase_cues_and_duration_on_integer_frames() {
        let mut timeline = OutputTimeline::default();
        timeline.observe(0, true).unwrap();
        timeline.observe(12, false).unwrap();
        timeline.observe(20, true).unwrap();
        timeline.observe(24, true).unwrap();
        timeline.observe(30, false).unwrap();
        timeline.observe(36, false).unwrap();
        assert_eq!(timeline.omitted, [(0, 12), (20, 30)]);
        for (scene, output) in [
            (0, 0),
            (12, 0),
            (13, 1),
            (20, 8),
            (25, 8),
            (30, 8),
            (36, 14),
        ] {
            assert_eq!(timeline.output_frame(scene).unwrap(), output);
        }
    }

    #[test]
    fn ordinary_output_preserves_every_timestamp() {
        let mut timeline = OutputTimeline::default();
        timeline.observe(0, false).unwrap();
        timeline.observe(60, false).unwrap();
        for frame in 0..=60 {
            assert_eq!(timeline.output_frame(frame).unwrap(), frame);
        }
        assert!(timeline.omitted.is_empty());
    }

    #[test]
    fn rewind_refuses_without_changing_the_output_map() {
        let mut timeline = OutputTimeline::default();
        timeline.observe(0, true).unwrap();
        timeline.observe(10, false).unwrap();
        assert!(timeline.observe(9, false).is_err());
        assert_eq!(timeline.output_frame(10).unwrap(), 0);
        assert!(timeline.output_frame(11).is_err());
        assert!(timeline.output_frame(-1).is_err());
    }

    #[test]
    fn production_portal_playback_selection_acceptance() {
        crate::with_python_test_module("playback selection acceptance", |py, _module, globals| {
            globals
                .set_item(
                    "__file__",
                    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/playback_selection.py"),
                )
                .expect("set playback source path");
            let source = CString::new(include_str!("../tests/playback_selection.py"))
                .expect("playback selection source contains no NUL");
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .expect("native playback selection acceptance suite");
        });
    }
}
