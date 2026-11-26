//! The readiness input of the gate, and the gate state that the gate and
//! `/health` read.
//!
//! ADR 0001 opens the gate only when four conditions are true: (1) the
//! child runs; (2) the SUB socket is connected and settled; (3) the
//! start-up check passed, or degraded mode applies; (4) the queue did not
//! close the gate. This module takes the conditions from their owners:
//!
//! - The child supervisor sets condition 1 with
//!   [`Readiness::set_child_running`].
//! - The pipeline (the ZMQ reader, the start-up check, and the queue) owns
//!   conditions 2 to 4. It reports them together as one [`Pipeline`] value
//!   with [`Readiness::set_pipeline`].
//!
//! `/health` answers 200 only when the gate is open and the pipeline is
//! [`Pipeline::Ready`]. Degraded mode opens the gate, but `/health` fails
//! (ADR 0001). [`Readiness::shut`] closes the gate for the rest of the
//! process at the start of the shutdown.
//!
//! Each change from open to closed increments the epoch of the state. A
//! forwarded connection keeps the epoch of its accept and ends when the
//! epoch changes, also when the gate opens again before the connection task
//! sees the closed state.
//!
//! Each change between open and closed records a `gate_opened` or
//! `gate_closed` event (ADR 0002) in the sink. Several tasks write the state
//! (the supervisor, the pipeline, and the shutdown); the event is recorded
//! inside the lock of the change, so the events keep the order of the
//! changes.
//!
//! The pipeline ([`crate::pipeline`]) is the only owner of the pipeline
//! value. Until its first report, the value is [`Pipeline::NotReady`] with
//! the reason [`PIPELINE_STARTING`], so the gate is closed and `/health`
//! answers 503.

use std::sync::Arc;

use tokio::sync::watch;

use crate::events::{EventKind, EventSink, IngestEvent};

/// The reason of the closed gate before the first report of the pipeline.
pub const PIPELINE_STARTING: &str = "pipeline_starting";

/// The state of gate conditions 2 to 4, from the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pipeline {
    /// A condition is not met. The gate stays closed. The static text names
    /// the condition, for example `subscriber_not_settled`.
    NotReady(&'static str),
    /// The pipeline takes records in degraded mode (ADR 0001, gate
    /// condition 3). The gate can open, but `/health` fails.
    Degraded,
    /// All pipeline conditions are met.
    Ready,
}

/// A snapshot of the gate conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateState {
    child_running: bool,
    pipeline: Pipeline,
    shut: bool,
    epoch: u64,
}

impl GateState {
    const INITIAL: Self = Self {
        child_running: false,
        pipeline: Pipeline::NotReady(PIPELINE_STARTING),
        shut: false,
        epoch: 0,
    };

    /// True when the gate forwards new connections.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.closed_reason().is_none()
    }

    /// True when `/health` answers 200: the gate is open and the pipeline is
    /// ready, not degraded.
    #[must_use]
    pub const fn is_healthy(&self) -> bool {
        self.is_open() && matches!(self.pipeline, Pipeline::Ready)
    }

    /// Gate condition 1: the child runs.
    #[must_use]
    pub const fn child_running(&self) -> bool {
        self.child_running
    }

    /// True after [`Readiness::shut`].
    #[must_use]
    pub const fn is_shut(&self) -> bool {
        self.shut
    }

    /// The number of changes from open to closed so far.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The first condition that keeps the gate closed, or `None` when the
    /// gate is open.
    #[must_use]
    pub const fn closed_reason(&self) -> Option<&'static str> {
        if self.shut {
            Some("shutdown")
        } else if !self.child_running {
            Some("child_not_running")
        } else if let Pipeline::NotReady(reason) = self.pipeline {
            Some(reason)
        } else {
            None
        }
    }
}

/// The owner of the gate state. Share it with [`Arc`].
pub struct Readiness {
    state: watch::Sender<GateState>,
    sink: Arc<dyn EventSink>,
}

impl Readiness {
    /// A closed gate: the child does not run and the pipeline is absent.
    #[must_use]
    pub fn new(sink: Arc<dyn EventSink>) -> Self {
        Self {
            state: watch::Sender::new(GateState::INITIAL),
            sink,
        }
    }

    /// Sets gate condition 1.
    pub fn set_child_running(&self, running: bool) {
        self.update(|state| state.child_running = running);
    }

    /// Sets gate conditions 2 to 4.
    pub fn set_pipeline(&self, pipeline: Pipeline) {
        self.update(|state| state.pipeline = pipeline);
    }

    /// Closes the gate for the rest of the process.
    pub fn shut(&self) {
        self.update(|state| state.shut = true);
    }

    /// The current state.
    #[must_use]
    pub fn state(&self) -> GateState {
        *self.state.borrow()
    }

    /// A receiver that sees each change of the state.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<GateState> {
        self.state.subscribe()
    }

    fn update(&self, change: impl FnOnce(&mut GateState)) {
        self.state.send_if_modified(|state| {
            let before = *state;
            change(state);
            if before.is_open() && !state.is_open() {
                state.epoch = state.epoch.wrapping_add(1);
            }
            if before.is_open() != state.is_open() {
                let kind = match state.closed_reason() {
                    None => EventKind::GateOpened,
                    Some(reason) => EventKind::GateClosed { reason },
                };
                // Record while the watch channel holds its lock, so that the
                // events of several writers keep the order of the changes.
                // The sink returns at once (`EventSink::record`).
                self.sink.record(IngestEvent::now(kind));
            }
            before != *state
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::recording::RecordingSink;

    fn readiness() -> (Readiness, RecordingSink) {
        let sink = RecordingSink::default();
        (Readiness::new(Arc::new(sink.clone())), sink)
    }

    #[test]
    fn the_gate_starts_closed_and_unhealthy() {
        let (readiness, sink) = readiness();
        let state = readiness.state();
        assert!(!state.is_open());
        assert!(!state.is_healthy());
        assert_eq!(state.closed_reason(), Some("child_not_running"));
        assert_eq!(sink.names(), [] as [&str; 0]);
    }

    #[test]
    fn the_running_child_alone_does_not_open_the_gate() {
        // Before the first report of the pipeline, the gate stays closed.
        let (readiness, sink) = readiness();
        readiness.set_child_running(true);
        assert!(readiness.state().child_running());
        assert!(!readiness.state().is_open());
        assert_eq!(readiness.state().closed_reason(), Some(PIPELINE_STARTING));
        assert_eq!(sink.names(), [] as [&str; 0]);
    }

    #[test]
    fn the_gate_opens_when_the_child_runs_and_the_pipeline_is_ready() {
        let (readiness, sink) = readiness();
        readiness.set_pipeline(Pipeline::Ready);
        assert!(!readiness.state().is_open());
        readiness.set_child_running(true);
        let state = readiness.state();
        assert!(state.is_open());
        assert!(state.is_healthy());
        assert_eq!(sink.names(), ["gate_opened"]);
    }

    #[test]
    fn degraded_mode_opens_the_gate_but_health_fails() {
        let (readiness, _sink) = readiness();
        readiness.set_child_running(true);
        readiness.set_pipeline(Pipeline::Degraded);
        assert!(readiness.state().is_open());
        assert!(!readiness.state().is_healthy());
    }

    #[test]
    fn each_close_records_the_reason_and_increments_the_epoch() {
        let (readiness, sink) = readiness();
        readiness.set_pipeline(Pipeline::Ready);
        readiness.set_child_running(true);
        assert_eq!(readiness.state().epoch(), 0);

        readiness.set_child_running(false);
        assert_eq!(readiness.state().epoch(), 1);
        readiness.set_child_running(true);
        readiness.set_pipeline(Pipeline::NotReady("queue_full"));
        assert_eq!(readiness.state().epoch(), 2);
        // A change while closed is not a new close.
        readiness.set_child_running(false);
        assert_eq!(readiness.state().epoch(), 2);

        let events = sink.events();
        let kinds: Vec<_> = events.iter().map(|event| event.kind.clone()).collect();
        assert_eq!(
            kinds,
            [
                EventKind::GateOpened,
                EventKind::GateClosed {
                    reason: "child_not_running"
                },
                EventKind::GateOpened,
                EventKind::GateClosed {
                    reason: "queue_full"
                },
            ]
        );
    }

    #[test]
    fn shut_closes_the_gate_for_good() {
        let (readiness, sink) = readiness();
        readiness.set_pipeline(Pipeline::Ready);
        readiness.set_child_running(true);
        readiness.shut();
        assert!(readiness.state().is_shut());
        assert_eq!(readiness.state().closed_reason(), Some("shutdown"));
        readiness.set_child_running(true);
        readiness.set_pipeline(Pipeline::Ready);
        assert!(!readiness.state().is_open());
        assert_eq!(sink.names(), ["gate_opened", "gate_closed"]);
    }

    #[test]
    fn the_events_of_several_writers_keep_the_order_of_the_changes() {
        let (readiness, sink) = readiness();
        readiness.set_pipeline(Pipeline::Ready);
        let readiness = Arc::new(readiness);
        let writers: Vec<_> = [true, false]
            .into_iter()
            .map(|running| {
                let readiness = Arc::clone(&readiness);
                std::thread::spawn(move || {
                    for _ in 0..2000 {
                        readiness.set_child_running(running);
                        readiness.set_child_running(!running);
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        // The state changes alternate between open and closed, so the
        // recorded events must alternate too.
        let names = sink.names();
        assert!(names.len() > 2);
        for pair in names.windows(2) {
            assert_ne!(pair[0], pair[1], "two equal events in a row");
        }
        let open = readiness.state().is_open();
        assert_eq!(names.last() == Some(&"gate_opened"), open);
    }

    #[tokio::test]
    async fn a_subscriber_sees_each_change() {
        let (readiness, _sink) = readiness();
        let mut receiver = readiness.subscribe();
        readiness.set_child_running(true);
        receiver.changed().await.unwrap();
        readiness.set_child_running(true);
        assert!(
            !receiver.has_changed().unwrap(),
            "an equal value is no change"
        );
    }
}
