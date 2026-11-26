//! The one owner of the [`Pipeline`] value: gate conditions 2 to 4 of ADR
//! 0001.
//!
//! Three parts of the pipeline report one condition each:
//!
//! | Condition | Reporter | Values |
//! | --------- | -------- | ------ |
//! | 2: the SUB socket is connected and settled | [`super::reader`] | settled, not settled, or stalled |
//! | 3: the start-up check passed, or degraded mode | [`super::writer`] | [`Database`] |
//! | 4: the queue did not close the gate | [`super::queue`] | below the high mark, or closed |
//!
//! Each report changes one field and sets the combined value in
//! [`Readiness`] while it holds the lock of this type. So the reports of
//! several tasks reach the gate state in the order of the changes, and a
//! late report cannot set an old value. The lock order is: the queue, this
//! type, the gate state, the event buffer. The event buffer never calls
//! back.
//!
//! The combined value names the first condition that is not met, in the
//! order of ADR 0001, as the reason of the closed gate: `subscriber_not_settled`,
//! `subscriber_stalled`,
//! `database_check_pending`, `database_refused`, or `queue_high_mark`.
//! Degraded mode with every other condition met gives
//! [`Pipeline::Degraded`]: the gate opens, and `/health` fails.
//!
//! The change to [`Database::Degraded`] records the `degraded_mode` event
//! (ADR 0002).

use std::sync::{Arc, Mutex, PoisonError};

use crate::events::{EventKind, EventSink, IngestEvent};
use crate::readiness::{Pipeline, Readiness};

/// Gate condition 3: the state of the start-up check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Database {
    /// No check has passed, and the time of no answer is below the limit
    /// of degraded mode.
    Pending,
    /// The check passed.
    Passed,
    /// The database gave no answer for the limit of degraded mode
    /// (ADR 0001, gate condition 3).
    Degraded,
    /// The database answered with a failure: a configuration fault. The
    /// gate stays closed.
    Refused,
}

/// Gate condition 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Subscriber {
    Settled,
    NotSettled,
    Stalled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct State {
    subscriber: Subscriber,
    database: Database,
    queue_closed: bool,
}

impl State {
    const fn pipeline(self) -> Pipeline {
        match self.subscriber {
            Subscriber::Settled => {}
            Subscriber::NotSettled => return Pipeline::NotReady("subscriber_not_settled"),
            Subscriber::Stalled => return Pipeline::NotReady("subscriber_stalled"),
        }
        let degraded = match self.database {
            Database::Pending => return Pipeline::NotReady("database_check_pending"),
            Database::Refused => return Pipeline::NotReady("database_refused"),
            Database::Degraded => true,
            Database::Passed => false,
        };
        if self.queue_closed {
            Pipeline::NotReady("queue_high_mark")
        } else if degraded {
            Pipeline::Degraded
        } else {
            Pipeline::Ready
        }
    }
}

/// The owner of gate conditions 2 to 4. Share it with [`Arc`].
pub struct Conditions {
    state: Mutex<State>,
    readiness: Arc<Readiness>,
    sink: Arc<dyn EventSink>,
}

impl Conditions {
    /// No condition is met: the SUB socket is not settled and the start-up
    /// check has not passed. Sets this value in `readiness` at once.
    #[must_use]
    pub fn new(readiness: Arc<Readiness>, sink: Arc<dyn EventSink>) -> Self {
        let state = State {
            subscriber: Subscriber::NotSettled,
            database: Database::Pending,
            queue_closed: false,
        };
        readiness.set_pipeline(state.pipeline());
        Self {
            state: Mutex::new(state),
            readiness,
            sink,
        }
    }

    /// Gate condition 2.
    pub fn set_settled(&self, settled: bool) {
        let subscriber = if settled {
            Subscriber::Settled
        } else {
            Subscriber::NotSettled
        };
        self.update(|state| state.subscriber = subscriber);
    }

    /// Gate condition 2 is false because the stall rule found that the
    /// child dispatched records and the reader received none (ADR 0001,
    /// "SUB socket"). The reason of the closed gate is
    /// `subscriber_stalled`, so the `gate_closed` event shows the stall.
    pub fn set_stalled(&self) {
        self.update(|state| state.subscriber = Subscriber::Stalled);
    }

    /// Gate condition 3. A change to [`Database::Degraded`] records the
    /// `degraded_mode` event.
    pub fn set_database(&self, database: Database) {
        self.update(|state| state.database = database);
    }

    /// Gate condition 4: true when the queue closed the gate.
    pub fn set_queue_closed(&self, closed: bool) {
        self.update(|state| state.queue_closed = closed);
    }

    /// The state of gate condition 3.
    #[must_use]
    pub fn database(&self) -> Database {
        self.lock().database
    }

    /// The combined value.
    #[must_use]
    pub fn pipeline(&self) -> Pipeline {
        self.lock().pipeline()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn update(&self, change: impl FnOnce(&mut State)) {
        let mut state = self.lock();
        let before = *state;
        change(&mut state);
        if state.database == Database::Degraded && before.database != Database::Degraded {
            self.sink.record(IngestEvent::now(EventKind::DegradedMode));
        }
        if state.pipeline() != before.pipeline() {
            // Inside the lock: the order of the reports is the order of the
            // changes (module documentation).
            self.readiness.set_pipeline(state.pipeline());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::recording::RecordingSink;

    fn conditions() -> (Conditions, Arc<Readiness>, RecordingSink) {
        let sink = RecordingSink::default();
        let readiness = Arc::new(Readiness::new(Arc::new(sink.clone())));
        readiness.set_child_running(true);
        (
            Conditions::new(Arc::clone(&readiness), Arc::new(sink.clone())),
            readiness,
            sink,
        )
    }

    #[test]
    fn the_gate_opens_only_when_every_condition_is_met() {
        let (conditions, readiness, _sink) = conditions();
        assert_eq!(
            readiness.state().closed_reason(),
            Some("subscriber_not_settled")
        );
        conditions.set_settled(true);
        assert_eq!(
            readiness.state().closed_reason(),
            Some("database_check_pending")
        );
        conditions.set_database(Database::Passed);
        assert!(readiness.state().is_open());
        assert!(readiness.state().is_healthy());
        conditions.set_queue_closed(true);
        assert_eq!(readiness.state().closed_reason(), Some("queue_high_mark"));
        conditions.set_queue_closed(false);
        assert!(readiness.state().is_healthy());
        conditions.set_settled(false);
        assert_eq!(
            readiness.state().closed_reason(),
            Some("subscriber_not_settled")
        );
    }

    #[test]
    fn degraded_mode_opens_the_gate_records_the_event_and_fails_health() {
        let (conditions, readiness, sink) = conditions();
        conditions.set_settled(true);
        conditions.set_database(Database::Degraded);
        assert!(readiness.state().is_open());
        assert!(!readiness.state().is_healthy());
        assert_eq!(conditions.pipeline(), Pipeline::Degraded);
        // The same state again records nothing.
        conditions.set_database(Database::Degraded);
        assert_eq!(sink.names(), ["degraded_mode", "gate_opened"]);

        // The queue closes the gate in degraded mode too.
        conditions.set_queue_closed(true);
        assert!(!readiness.state().is_open());
        conditions.set_queue_closed(false);

        // An answered failure closes the gate at once.
        conditions.set_database(Database::Refused);
        assert_eq!(readiness.state().closed_reason(), Some("database_refused"));
        conditions.set_database(Database::Passed);
        assert!(readiness.state().is_healthy());
    }

    #[test]
    fn a_stalled_subscription_closes_the_gate_with_its_own_reason() {
        let (conditions, readiness, sink) = conditions();
        conditions.set_settled(true);
        conditions.set_database(Database::Passed);
        conditions.set_stalled();
        assert_eq!(
            readiness.state().closed_reason(),
            Some("subscriber_stalled")
        );
        // A new socket that waits for its settle time keeps the gate closed.
        conditions.set_settled(false);
        conditions.set_settled(true);
        assert!(readiness.state().is_healthy());
        let events = sink.events();
        let reasons: Vec<_> = events
            .iter()
            .filter_map(|event| match event.kind {
                EventKind::GateClosed { reason } => Some(reason),
                _ => None,
            })
            .collect();
        assert_eq!(reasons, ["subscriber_stalled"]);
    }

    #[test]
    fn reports_from_several_threads_end_in_the_last_value() {
        let (conditions, readiness, _sink) = conditions();
        conditions.set_database(Database::Passed);
        let conditions = Arc::new(conditions);
        let writers: Vec<_> = [true, false]
            .into_iter()
            .map(|first| {
                let conditions = Arc::clone(&conditions);
                std::thread::spawn(move || {
                    for _ in 0..1000 {
                        conditions.set_settled(first);
                        conditions.set_queue_closed(first);
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        conditions.set_settled(true);
        conditions.set_queue_closed(false);
        assert_eq!(conditions.pipeline(), Pipeline::Ready);
        assert!(readiness.state().is_healthy());
    }
}
