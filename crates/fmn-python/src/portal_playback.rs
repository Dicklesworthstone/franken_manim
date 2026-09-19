//! Portal playback policy over Choreo's clock and Reel's output timeline.
//!
//! Skipping changes capture and endpoint work, never invents a second clock.
//! Audio cue timestamps remain scene-relative in Proscenium; only the output
//! adapter removes skipped intervals when submitting cues to the native mixer.

use super::*;

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
    let skipping = requested_skip(scene)?;
    let engine = Rc::clone(&scene.borrow().engine);
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

#[cfg(test)]
mod tests {
    use super::*;

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
