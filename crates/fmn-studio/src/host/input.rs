//! Generation-bound, journalled input at the authenticated host boundary.
use super::*;
use crate::protocol::{StudioInput, studio_input_command};

pub(super) fn has_guard(form: &HashMap<String, String>) -> bool {
    ["worker_generation", "frame", "revision", "target"]
        .iter()
        .any(|key| form.contains_key(*key))
}

fn refused(message: &str) -> WorkerResponse {
    WorkerResponse::Error {
        code: WorkerErrorCode::InvalidRequest,
        message: message.to_owned(),
    }
}

fn bounded_event(event: &EventPayload) -> bool {
    const MAX: f64 = 1_000_000.0;
    let bounded = |values: &[f64]| {
        values
            .iter()
            .all(|value| value.is_finite() && value.abs() <= MAX)
    };
    match event {
        EventPayload::MouseMotion { point, delta, .. }
        | EventPayload::MouseDrag { point, delta, .. } => bounded(point) && bounded(delta),
        EventPayload::MousePress { point, .. } | EventPayload::MouseRelease { point, .. } => {
            bounded(point)
        }
        EventPayload::MouseScroll { point, offset, .. } => bounded(point) && bounded(offset),
        EventPayload::KeyPress { .. } | EventPayload::KeyRelease { .. } => true,
    }
}

impl StudioWorkerSession {
    /// Require generation, frame and optimistic revision on every HTTP event.
    /// The shipped host enables this. Existing embedders may still explicitly
    /// use unguarded transient previews by retaining the constructor default.
    #[must_use]
    pub fn require_guarded_input(mut self) -> Self {
        self.guarded_input = true;
        self
    }

    pub(super) fn inspect_generation(&self) -> Result<(WorkerResponse, u64), HostError> {
        let mut supervisor = lock(&self.supervisor);
        let response = worker_response(supervisor.request(
            SupervisorRequest::Inspect {
                scene: self.try_owned_scene()?,
            },
            &*self.asset_ok,
        )?)?;
        Ok((response, supervisor.generation()))
    }

    pub(super) fn guarded_event(
        &self,
        form: &HashMap<String, String>,
        event: EventPayload,
    ) -> Result<WorkerResponse, HostError> {
        if ["worker_generation", "frame", "revision"]
            .iter()
            .any(|key| !form.contains_key(*key))
        {
            return Ok(refused(
                "no live command/event adapter for unguarded input; worker_generation, frame and revision are required",
            ));
        }
        let generation = parse_required::<u64>(form, "worker_generation")?;
        let frame = parse_required::<i64>(form, "frame")?;
        let revision = parse_required::<u64>(form, "revision")?;
        let target = form
            .get("target")
            .map(|_| parse_required::<u64>(form, "target"))
            .transpose()?;
        if frame < 0 {
            return Err(HostError::BadRequest("negative input frame"));
        }
        if !bounded_event(&event) {
            return Ok(refused("native input exceeds the coordinate/delta budget"));
        }
        let scene = self.try_owned_scene()?;
        let command = studio_input_command(
            &scene,
            &StudioInput {
                frame,
                revision,
                target,
                event,
            },
        )?;
        // Do not release this lock between generation admission and dispatch:
        // an old browser event must never reach a replacement worker.
        let mut supervisor = lock(&self.supervisor);
        if generation != supervisor.generation() {
            return Err(HostError::BadRequest("stale native worker generation"));
        }
        let response = worker_response(supervisor.request(
            SupervisorRequest::Play {
                scene: self.try_owned_scene()?,
                command,
            },
            &*self.asset_ok,
        )?)?;
        if matches!(response, WorkerResponse::Error { .. }) {
            return Ok(response);
        }
        if !matches!(response, WorkerResponse::JournalSegment { .. }) {
            return Err(HostError::UnexpectedWorkerResponse);
        }
        self.committed_frame.store(frame, Ordering::Release);
        worker_response(
            supervisor.request(SupervisorRequest::Scrub { scene, frame }, &*self.asset_ok)?,
        )
    }
}
