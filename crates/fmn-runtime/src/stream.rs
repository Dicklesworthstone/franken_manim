//! Demand-driven entry to the existing bounded frame pipeline.
//!
//! Imperative front doors cannot express `Scene.play()` as a Send iterator:
//! their arena and callbacks are confined to the calling thread. The pipeline
//! requests one source value only AFTER it has room, and the caller freezes
//! that value under a [`FramePermit`]. There is no second queue of unaccounted
//! frames and no second scheduler. Closing drains; dropping cancels and joins.

use crate::{
    CancellationToken, ExecutionPlan, FramePipeline, PipelineEvent, PipelineFailure, PipelineStages,
    PipelineStats,
};
use std::fmt;
use std::marker::PhantomData;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

// Cancellation must wake a coordinator waiting for an imperative front door,
// not just its raster workers. This is a stop-check interval, not frame timing.
const STOP_CHECK: Duration = Duration::from_millis(10);

type Ticket<F> = SyncSender<PipelineEvent<F, ()>>;

/// Failure at the imperative pipeline boundary.
#[derive(Debug)]
pub enum FrameStreamError<E> {
    /// The host refused the coordinator thread.
    Spawn(std::io::Error),
    /// The existing pipeline reported a stage failure after joining workers.
    Pipeline(PipelineFailure<E>),
    /// The source closed or a worker failed. [`FrameStream::finish`] preserves
    /// the underlying pipeline failure, including its counters.
    Closed,
    /// The coordinator itself panicked outside the stage containment boundary.
    CoordinatorPanicked,
}

impl<E: fmt::Display> fmt::Display for FrameStreamError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "frame coordinator could not start: {error}"),
            Self::Pipeline(error) => error.fmt(f),
            Self::Closed => f.write_str("frame pipeline is closed or cancelled"),
            Self::CoordinatorPanicked => f.write_str("frame pipeline coordinator panicked"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for FrameStreamError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) => Some(error),
            Self::Pipeline(error) => Some(error),
            Self::Closed | Self::CoordinatorPanicked => None,
        }
    }
}

/// A single-owner, imperative producer for [`FramePipeline`].
///
/// `reserve` waits BEFORE source state is frozen. Native stages and ordered
/// output run on the pipeline's own workers/coordinator; the live front door
/// stays on its caller. No callback or arena crosses this interface.
/// Stage callbacks must cooperate with cancellation, as for `FramePipeline`.
/// A sink that can block must be cancelled before dropping this stream.
pub struct FrameStream<F: Send + 'static, E: Send + 'static> {
    requests: Option<Receiver<Ticket<F>>>,
    cancellation: CancellationToken,
    worker: Option<JoinHandle<Result<PipelineStats, PipelineFailure<E>>>>,
}

impl<F: Send + 'static, E: Send + 'static> FrameStream<F, E> {
    /// Start the existing scheduler with an owned plan and worker stages.
    ///
    /// # Errors
    /// Returns a spawn error without running any front-door callbacks.
    pub fn new<S, Emit>(
        plan: ExecutionPlan,
        stages: S,
        emit: Emit,
    ) -> Result<Self, FrameStreamError<E>>
    where
        S: PipelineStages<Frame = F, Error = E> + Send + 'static,
        S::Prepared: 'static,
        S::Rasterized: 'static,
        S::Output: 'static,
        Emit: FnMut(u64, S::Output) -> Result<(), E> + Send + 'static,
    {
        let cancellation = CancellationToken::new();
        let worker_cancel = cancellation.clone();
        // Buffer a demand ticket, never a frozen frame. This avoids polling
        // or an artificial per-frame delay when the front door is idle.
        let (requests, receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("fmn-frame-coordinator".into())
            .spawn(move || {
                let source = Source {
                    requests,
                    cancellation: worker_cancel.clone(),
                };
                FramePipeline::with_cancellation(&plan, &stages, worker_cancel)
                    .run(source, emit, |(), _| Ok(()))
            })
            .map_err(FrameStreamError::Spawn)?;
        Ok(Self {
            requests: Some(receiver),
            cancellation,
            worker: Some(worker),
        })
    }

    /// Wait for capacity without creating a frozen source value.
    ///
    /// The returned permit borrows this stream exclusively. Dropping a permit
    /// without submitting aborts the stream, rather than silently omitting a
    /// semantic frame. `finish` cannot race an outstanding permit.
    ///
    /// # Errors
    /// Returns [`FrameStreamError::Closed`] after failure or cancellation.
    pub fn reserve(&mut self) -> Result<FramePermit<'_, F>, FrameStreamError<E>> {
        let receiver = self.requests.as_ref().ok_or(FrameStreamError::Closed)?;
        loop {
            if self.cancellation.is_cancelled() {
                return Err(FrameStreamError::Closed);
            }
            match receiver.recv_timeout(STOP_CHECK) {
                Ok(sender) => {
                    return Ok(FramePermit {
                        sender: Some(sender),
                        cancellation: self.cancellation.clone(),
                        owner: PhantomData,
                    });
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Err(FrameStreamError::Closed),
            }
        }
    }

    /// Clone the cooperative cancellation flag (never a publication permit).
    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Close input, drain earlier frames in order, and join every worker.
    ///
    /// This only completes the pipeline; the caller still owns any sink's
    /// atomic artifact publication and must finalize it separately.
    ///
    /// # Errors
    /// Preserves the pipeline's first failure and final resource counters.
    pub fn finish(mut self) -> Result<PipelineStats, FrameStreamError<E>> {
        self.requests.take();
        self.join()
    }

    /// Cancel input/work, close the source, and join every worker.
    ///
    /// # Errors
    /// Returns cancellation or an earlier pipeline failure after joining.
    pub fn cancel(mut self) -> Result<PipelineStats, FrameStreamError<E>> {
        self.cancellation.cancel();
        self.requests.take();
        self.join()
    }

    fn join(&mut self) -> Result<PipelineStats, FrameStreamError<E>> {
        match self.worker.take().ok_or(FrameStreamError::Closed)?.join() {
            Ok(result) => result.map_err(FrameStreamError::Pipeline),
            Err(_) => Err(FrameStreamError::CoordinatorPanicked),
        }
    }
}

impl<F: Send + 'static, E: Send + 'static> Drop for FrameStream<F, E> {
    fn drop(&mut self) {
        if self.worker.is_some() {
            self.cancellation.cancel();
            self.requests.take();
            let _ = self.join();
        }
    }
}

/// Permission to freeze exactly one source frame on its existing owner.
#[must_use = "dropping an unsubmitted frame permit cancels the stream"]
pub struct FramePermit<'a, F> {
    sender: Option<Ticket<F>>,
    cancellation: CancellationToken,
    owner: PhantomData<&'a mut ()>,
}

impl<F> FramePermit<'_, F> {
    /// Transfer one frozen job.
    ///
    /// # Errors
    /// A disconnected receiver means the pipeline failed; the owner must join
    /// it to recover the detailed stage failure. The unsent frame is returned.
    pub fn submit(
        mut self,
        sequence: u64,
        frame: F,
    ) -> Result<(), mpsc::SendError<PipelineEvent<F, ()>>> {
        // A permit is constructed with its sender and consumed exactly once.
        let Some(sender) = self.sender.take() else {
            unreachable!("consumed frame permit")
        };
        sender.send(PipelineEvent::frame(sequence, frame))
    }
}

impl<F> Drop for FramePermit<'_, F> {
    fn drop(&mut self) {
        if self.sender.is_some() {
            self.cancellation.cancel();
        }
    }
}

struct Source<F> {
    requests: SyncSender<Ticket<F>>,
    cancellation: CancellationToken,
}

impl<F> Iterator for Source<F> {
    type Item = PipelineEvent<F, ()>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cancellation.is_cancelled() {
            return None;
        }
        let (sender, receiver) = mpsc::sync_channel(0);
        // At most one ticket exists: next() cannot run again until this
        // ticket is answered. Sending never waits for an idle front door.
        self.requests.send(sender).ok()?;
        loop {
            if self.cancellation.is_cancelled() {
                return None;
            }
            match receiver.recv_timeout(STOP_CHECK) {
                Ok(frame) => return Some(frame),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OutputPixelFormat, PipelineError, PlanRequest, RenderIntent, SurfaceSpec, TeamPlan};
    use fmn_platform::topology::HardwareTopology;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    struct Tracked {
        value: u64,
        alive: Arc<AtomicUsize>,
    }

    impl Drop for Tracked {
        fn drop(&mut self) {
            self.alive.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct Stages {
        fail: Option<u64>,
        panic: bool,
    }

    impl PipelineStages for Stages {
        type Frame = Tracked;
        type Prepared = Tracked;
        type Rasterized = Tracked;
        type Output = Tracked;
        type Error = String;

        fn prepare(&self, frame: Tracked, _: &TeamPlan) -> Result<Tracked, String> {
            Ok(frame)
        }

        fn rasterize(&self, frame: Tracked, _: &TeamPlan) -> Result<Tracked, String> {
            if self.fail == Some(frame.value) {
                assert!(!self.panic, "deliberate raster panic");
                return Err("deliberate raster failure".into());
            }
            if frame.value % 3 == 0 {
                thread::sleep(Duration::from_millis(2));
            }
            Ok(frame)
        }

        fn convert(&self, frame: Tracked, _: &TeamPlan) -> Result<Tracked, String> {
            Ok(frame)
        }
    }

    fn plan(slots: usize) -> ExecutionPlan {
        ExecutionPlan::derive(
            PlanRequest::certified(
                RenderIntent::Offline,
                SurfaceSpec::lumen(8, 4),
                OutputPixelFormat::Rgba8,
            )
            .with_max_frames_in_flight(slots),
            &HardwareTopology::fallback(8),
            None,
        )
        .unwrap()
    }

    fn tracked(value: u64, alive: &Arc<AtomicUsize>) -> Tracked {
        alive.fetch_add(1, Ordering::SeqCst);
        Tracked {
            value,
            alive: alive.clone(),
        }
    }

    fn stream(slots: usize, fail: Option<u64>, panic: bool) -> FrameStream<Tracked, String> {
        FrameStream::new(plan(slots), Stages { fail, panic }, |_, _| Ok(())).unwrap()
    }

    #[test]
    fn demand_counts_frozen_values_and_orders_every_output() {
        for slots in [1, 2, 4] {
            let plan = plan(slots);
            let limit = plan.frames_in_flight;
            let alive = Arc::new(AtomicUsize::new(0));
            let output = Arc::new(Mutex::new(Vec::new()));
            let received = output.clone();
            let mut stream = FrameStream::new(
                plan,
                Stages {
                    fail: None,
                    panic: false,
                },
                move |sequence, frame| {
                    received.lock().unwrap().push((sequence, frame.value));
                    Ok(())
                },
            )
            .unwrap();
            for value in 0..24 {
                let permit = stream.reserve().unwrap();
                let frame = tracked(value, &alive);
                assert!(alive.load(Ordering::SeqCst) <= limit);
                assert!(permit.submit(value, frame).is_ok());
            }
            let stats = stream.finish().unwrap();
            assert_eq!(stats.emitted, 24);
            assert_eq!(stats.outstanding_slots, 0);
            assert!(stats.max_in_flight <= limit);
            assert_eq!(alive.load(Ordering::SeqCst), 0);
            assert_eq!(
                *output.lock().unwrap(),
                (0..24).map(|n| (n, n)).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn abandoned_source_permit_cancels_and_joins() {
        let mut stream = stream(2, None, false);
        drop(stream.reserve().unwrap());
        assert!(matches!(
            stream.finish(),
            Err(FrameStreamError::Pipeline(PipelineFailure {
                error: PipelineError::Cancelled,
                ..
            }))
        ));
    }

    #[test]
    fn stage_failure_and_panic_preserve_diagnostics_and_release_frames() {
        for panic in [false, true] {
            let alive = Arc::new(AtomicUsize::new(0));
            let mut stream = stream(2, Some(0), panic);
            let permit = stream.reserve().unwrap();
            assert!(permit.submit(0, tracked(0, &alive)).is_ok());
            let Err(FrameStreamError::Pipeline(failure)) = stream.finish() else {
                panic!("lost failure")
            };
            if panic {
                assert!(matches!(
                    failure.error,
                    PipelineError::CallbackPanicked { .. }
                ));
            } else {
                assert!(matches!(failure.error, PipelineError::Stage { .. }));
            }
            assert_eq!(failure.stats.outstanding_slots, 0);
            assert_eq!(alive.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn empty_finish_and_idle_cancellation_do_not_hang() {
        assert_eq!(stream(1, None, false).finish().unwrap().emitted, 0);
        assert!(stream(1, None, false).cancel().is_err());
    }

    #[test]
    fn duplicate_sequence_is_refused_without_leaking_the_unsent_tail() {
        let alive = Arc::new(AtomicUsize::new(0));
        let mut stream = stream(2, None, false);
        for value in [7, 7] {
            let permit = stream.reserve().unwrap();
            assert!(permit.submit(value, tracked(value, &alive)).is_ok());
        }
        let Err(FrameStreamError::Pipeline(failure)) = stream.finish() else {
            panic!("duplicate frame was accepted")
        };
        assert!(matches!(
            failure.error,
            PipelineError::NonMonotonicSequence {
                previous: 7,
                next: 7
            }
        ));
        assert_eq!(failure.stats.outstanding_slots, 0);
        assert_eq!(alive.load(Ordering::SeqCst), 0);
    }
}
