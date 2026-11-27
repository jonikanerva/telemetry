//! The `ingest_event` records of ADR 0002 and the sink that takes them.
//!
//! ADR 0002 stores each event of the process in `raw.ingest_event`. The
//! gate, the readiness input, the child supervisor, and the pipeline make
//! these events. ADR 0001 keeps the events in memory until the writer
//! stores them, like the records. The production sink is [`EventBuffer`].
//!
//! Bounds: [`EventSink::record`] must return at once. It must not wait for
//! I/O or for a lock that another task holds for a long time, because the
//! gate acceptor and the supervisor call it inside their loops. The sink
//! owns the memory bound of the events that it keeps.
//!
//! [`crate::readiness::Readiness`] calls `record` while it holds the lock of
//! the gate state, so that the events keep the order of the changes. A sink
//! must therefore not call back into `Readiness` from `record` (for example
//! to close the gate at the high mark of a queue): that call would wait for
//! the same lock on the same thread. Change the gate state after `record`
//! returns. And a `record` that blocks also blocks every reader of the gate
//! state; the watchdog then ends the process. For this reason
//! [`EventBuffer::record`] does not write the log line of the event: a
//! blocked standard output would block the gate state. The log task
//! ([`EventBuffer::run_log`]) writes it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use tokio::sync::Notify;
use tokio::time::Instant;
use tracing::{info, warn};

/// One `ingest_event` record without the run identifier. The writer adds
/// the run identifier (ADR 0002).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestEvent {
    /// The time of the event, from the wall clock of the process.
    pub occurred_at: Timestamp,
    /// The kind and the data of the event.
    pub kind: EventKind,
}

impl IngestEvent {
    /// Makes an event that occurred now.
    #[must_use]
    pub fn now(kind: EventKind) -> Self {
        Self {
            occurred_at: Timestamp::now(),
            kind,
        }
    }
}

/// The reason of a `records_dropped` event: the reader found the queue
/// full (ADR 0001, "Delivery path", step 3).
pub const DROPPED_QUEUE_FULL: &str = "queue_full";

/// The reason of a `records_dropped` event: the database refused a record
/// and also its `unprocessed_record` row, so its bytes cannot be stored
/// (the defect case of [`crate::storage`]).
pub const DROPPED_DATABASE_REFUSED: &str = "database_refused";

/// The kinds of ADR 0002 (`raw.ingest_event.kind`) with their data.
///
/// The variants with a [`CountWindow`] are the kinds that the CHECK
/// constraint of ADR 0002 requires to have a count and both times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    /// The process started.
    Started,
    /// The shutdown sequence ran. The reason is the cause of the stop.
    Stopped {
        /// Static text, for example `SIGTERM`.
        reason: &'static str,
    },
    /// The gate opened.
    GateOpened,
    /// The gate closed.
    GateClosed {
        /// Static text: the first gate condition that is not met.
        reason: &'static str,
    },
    /// The pipeline entered degraded mode (ADR 0001, gate condition 3).
    DegradedMode,
    /// The child process ended, or it could not start.
    ChildExited {
        /// How the child ended.
        exit: ChildExit,
    },
    /// Records that were received and not stored.
    RecordsDropped {
        /// The count and the receive times of the dropped records.
        window: CountWindow,
        /// [`DROPPED_QUEUE_FULL`] or [`DROPPED_DATABASE_REFUSED`].
        reason: &'static str,
    },
    /// The shortfall check found messages that the reader did not receive.
    ZmqShortfall(CountWindow),
    /// The gate refused connections at its connection bound.
    GateFull(CountWindow),
}

impl EventKind {
    /// The value of the `kind` column.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Stopped { .. } => "stopped",
            Self::GateOpened => "gate_opened",
            Self::GateClosed { .. } => "gate_closed",
            Self::DegradedMode => "degraded_mode",
            Self::ChildExited { .. } => "child_exited",
            Self::RecordsDropped { .. } => "records_dropped",
            Self::ZmqShortfall(_) => "zmq_shortfall",
            Self::GateFull(_) => "gate_full",
        }
    }

    /// The value of the `reason` column, if the kind has one. The text holds
    /// static words and numbers only.
    #[must_use]
    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Stopped { reason }
            | Self::GateClosed { reason }
            | Self::RecordsDropped { reason, .. } => Some((*reason).to_owned()),
            Self::ChildExited { exit } => Some(exit.to_string()),
            _ => None,
        }
    }

    /// The count and the time window, if the kind has them.
    #[must_use]
    pub const fn window(&self) -> Option<&CountWindow> {
        match self {
            Self::RecordsDropped { window, .. }
            | Self::ZmqShortfall(window)
            | Self::GateFull(window) => Some(window),
            _ => None,
        }
    }
}

/// How the child process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChildExit {
    /// The child exited with this code.
    #[error("exit code {0}")]
    Code(i32),
    /// A signal with this number ended the child.
    #[error("signal {0}")]
    Signal(i32),
    /// The operating system gave no code and no signal.
    #[error("unknown status")]
    Unknown,
    /// The child could not start.
    #[error("spawn failed")]
    SpawnFailed,
    /// The wait for the child failed.
    #[error("wait failed")]
    WaitFailed,
}

/// A count of items and the times of the first and the last item.
///
/// `first <= last` holds when the window is built with [`CountWindow::one`]
/// and [`CountWindow::add`] from a clock that does not go back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CountWindow {
    /// Number of items. At least 1.
    pub count: u32,
    /// Time of the first item.
    pub first: Timestamp,
    /// Time of the last item.
    pub last: Timestamp,
}

impl CountWindow {
    /// A window with one item at `at`.
    #[must_use]
    pub const fn one(at: Timestamp) -> Self {
        Self {
            count: 1,
            first: at,
            last: at,
        }
    }

    /// Adds one item at `at`. The count stops at `u32::MAX`.
    pub fn add(&mut self, at: Timestamp) {
        self.count = self.count.saturating_add(1);
        self.last = self.last.max(at);
    }

    /// True when the count cannot grow any more.
    #[must_use]
    pub const fn is_full(&self) -> bool {
        self.count == u32::MAX
    }
}

/// Counts items and gives their window at most once each interval: at the
/// first item, then at most once each `interval` with the items since the
/// last window. The `gate_full` events of the gate and the
/// `records_dropped` events of the reader use it.
#[derive(Debug)]
pub struct CountThrottle {
    interval: Duration,
    last_record: Option<Instant>,
    pending: Option<CountWindow>,
}

impl CountThrottle {
    /// A throttle with nothing counted.
    #[must_use]
    pub const fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_record: None,
            pending: None,
        }
    }

    /// Counts one item at `at` and returns the window to record, if one is
    /// due. A window whose count cannot grow is due at once.
    pub fn count(&mut self, now: Instant, at: Timestamp) -> Option<CountWindow> {
        match &mut self.pending {
            Some(window) if window.is_full() => {
                let full = *window;
                *window = CountWindow::one(at);
                self.last_record = Some(now);
                return Some(full);
            }
            Some(window) => window.add(at),
            None => self.pending = Some(CountWindow::one(at)),
        }
        self.due(now)
    }

    /// Returns the counted window when the interval since the last record
    /// has passed.
    pub fn due(&mut self, now: Instant) -> Option<CountWindow> {
        self.pending?;
        if let Some(last) = self.last_record
            && now.saturating_duration_since(last) < self.interval
        {
            return None;
        }
        self.last_record = Some(now);
        self.pending.take()
    }

    /// Returns the counted window without the rate limit, at the stop.
    pub const fn take(&mut self) -> Option<CountWindow> {
        self.pending.take()
    }
}

/// The receiver of the `ingest_event` records.
pub trait EventSink: Send + Sync {
    /// Takes one event. Returns at once, does not block, and does not call
    /// into [`crate::readiness::Readiness`] (module documentation).
    fn record(&self, event: IngestEvent);
}

/// The production sink: a bounded buffer of the events that wait for the
/// writer (ADR 0001: the events wait in the memory queue), and of the
/// events that wait for the log.
///
/// - **Bound.** At most `capacity` events wait for the writer, and at most
///   `capacity` events wait for the log. A new event beyond the bound is
///   not kept, and a count shows it: [`EventBuffer::not_kept`] for the
///   writer, the log event `events_not_logged` for the log. The `stopped`
///   event is always kept, so that the final flush can store it. With the
///   production bound of 10,000 events, each about 100 bytes, the buffer
///   holds about 1 MiB.
/// - **Order and key.** `raw.ingest_event` has the key `(run_id, kind,
///   occurred_at)` in microseconds. Two events of one kind in the same
///   microsecond would be one row. The buffer therefore makes the times
///   strictly increasing in microseconds: an event that is not later than
///   the event before it gets the time of that event plus 1 µs. The same
///   rule keeps the order of the events when the wall clock goes back.
/// - **Wake.** `record` notifies the writer when the buffer was empty, so
///   that the writer sees the age of the oldest event (the flush interval
///   applies to events too), and it notifies the log task.
pub struct EventBuffer {
    inner: Mutex<Buffer>,
    capacity: usize,
    write_wake: Arc<Notify>,
    log_wake: Notify,
}

#[derive(Default)]
struct Buffer {
    to_write: VecDeque<Waiting>,
    to_log: VecDeque<IngestEvent>,
    not_kept: u64,
    not_logged: u64,
    last_micros: Option<i64>,
}

struct Waiting {
    event: IngestEvent,
    since: Instant,
}

impl EventBuffer {
    /// An empty buffer that keeps at most `capacity` events for the writer
    /// and for the log. `write_wake` is the wake of the writer.
    #[must_use]
    pub fn new(capacity: usize, write_wake: Arc<Notify>) -> Self {
        Self {
            inner: Mutex::new(Buffer::default()),
            capacity,
            write_wake,
            log_wake: Notify::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Buffer> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The time when the oldest event that waits for the writer arrived.
    #[must_use]
    pub fn oldest(&self) -> Option<Instant> {
        self.lock().to_write.front().map(|waiting| waiting.since)
    }

    /// The number of events that wait for the writer.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().to_write.len()
    }

    /// True when no event waits for the writer.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A copy of the events that wait for the writer, oldest first. The
    /// writer removes them with [`EventBuffer::remove`] after the commit.
    #[must_use]
    pub fn pending(&self) -> Vec<IngestEvent> {
        self.lock()
            .to_write
            .iter()
            .map(|waiting| waiting.event.clone())
            .collect()
    }

    /// Removes the `count` oldest events that wait for the writer.
    pub fn remove(&self, count: usize) {
        let mut buffer = self.lock();
        let count = count.min(buffer.to_write.len());
        buffer.to_write.drain(..count);
    }

    /// The number of events that the buffer did not keep for the writer.
    #[must_use]
    pub fn not_kept(&self) -> u64 {
        self.lock().not_kept
    }

    /// Writes the log line of each event that waits for the log. The log
    /// task calls it; the shutdown calls it once more at its end.
    pub fn log_pending(&self) {
        let (events, not_logged) = {
            let mut buffer = self.lock();
            let events: Vec<IngestEvent> = buffer.to_log.drain(..).collect();
            (events, std::mem::take(&mut buffer.not_logged))
        };
        for event in events {
            let kind = event.kind.name();
            let reason = event.kind.reason();
            let count = event.kind.window().map(|window| window.count);
            info!(kind, reason = reason.as_deref(), count, "ingest_event");
        }
        if not_logged > 0 {
            warn!(count = not_logged, "events_not_logged");
        }
    }

    /// The log task: writes the log lines of the events as they arrive.
    /// It runs until the process ends.
    pub async fn run_log(self: Arc<Self>) {
        loop {
            self.log_wake.notified().await;
            self.log_pending();
        }
    }
}

impl EventSink for EventBuffer {
    fn record(&self, mut event: IngestEvent) {
        let mut buffer = self.lock();
        let micros = event.occurred_at.as_microsecond();
        if let Some(last) = buffer.last_micros
            && micros <= last
            && let Ok(later) = Timestamp::from_microsecond(last.saturating_add(1))
        {
            event.occurred_at = later;
        }
        buffer.last_micros = Some(event.occurred_at.as_microsecond().max(micros));
        if buffer.to_log.len() < self.capacity {
            buffer.to_log.push_back(event.clone());
        } else {
            buffer.not_logged += 1;
        }
        let was_empty = buffer.to_write.is_empty();
        if buffer.to_write.len() < self.capacity || matches!(event.kind, EventKind::Stopped { .. })
        {
            buffer.to_write.push_back(Waiting {
                event,
                since: Instant::now(),
            });
        } else {
            buffer.not_kept += 1;
        }
        drop(buffer);
        // `Notify` takes no lock that another task holds for a long time, and
        // it stores the wake when no task waits.
        if was_empty {
            self.write_wake.notify_one();
        }
        self.log_wake.notify_one();
    }
}

/// A sink that keeps every event in memory, for tests.
#[cfg(test)]
pub(crate) mod recording {
    use std::sync::{Arc, Mutex};

    use super::{EventSink, IngestEvent};

    /// Keeps every event. Clones share the same list.
    #[derive(Debug, Clone, Default)]
    pub(crate) struct RecordingSink(Arc<Mutex<Vec<IngestEvent>>>);

    impl RecordingSink {
        pub(crate) fn events(&self) -> Vec<IngestEvent> {
            self.0.lock().unwrap().clone()
        }

        pub(crate) fn names(&self) -> Vec<&'static str> {
            self.events()
                .iter()
                .map(|event| event.kind.name())
                .collect()
        }
    }

    impl EventSink for RecordingSink {
        fn record(&self, event: IngestEvent) {
            self.0.lock().unwrap().push(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::{ALLOWED_FIELDS, capture};

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    #[test]
    fn kind_names_match_adr_0002() {
        let window = CountWindow::one(at(1));
        let kinds = [
            (EventKind::Started, "started"),
            (EventKind::Stopped { reason: "SIGTERM" }, "stopped"),
            (EventKind::GateOpened, "gate_opened"),
            (EventKind::GateClosed { reason: "x" }, "gate_closed"),
            (EventKind::DegradedMode, "degraded_mode"),
            (
                EventKind::ChildExited {
                    exit: ChildExit::Code(1),
                },
                "child_exited",
            ),
            (
                EventKind::RecordsDropped {
                    window,
                    reason: DROPPED_QUEUE_FULL,
                },
                "records_dropped",
            ),
            (EventKind::ZmqShortfall(window), "zmq_shortfall"),
            (EventKind::GateFull(window), "gate_full"),
        ];
        for (kind, name) in kinds {
            assert_eq!(kind.name(), name);
            // ADR 0002 CHECK: these three kinds have a count and both times.
            let counted = matches!(name, "records_dropped" | "zmq_shortfall" | "gate_full");
            assert_eq!(kind.window().is_some(), counted, "{name}");
        }
    }

    #[test]
    fn reasons_are_static_words_and_numbers() {
        let reason = |exit| EventKind::ChildExited { exit }.reason().unwrap();
        assert_eq!(reason(ChildExit::Code(3)), "exit code 3");
        assert_eq!(reason(ChildExit::Signal(9)), "signal 9");
        assert_eq!(reason(ChildExit::Unknown), "unknown status");
        assert_eq!(reason(ChildExit::SpawnFailed), "spawn failed");
        assert_eq!(reason(ChildExit::WaitFailed), "wait failed");
        assert_eq!(EventKind::Started.reason(), None);
        let dropped = EventKind::RecordsDropped {
            window: CountWindow::one(at(1)),
            reason: DROPPED_DATABASE_REFUSED,
        };
        assert_eq!(dropped.reason().as_deref(), Some("database_refused"));
    }

    #[test]
    fn a_window_counts_and_keeps_the_first_and_the_last_time() {
        let mut window = CountWindow::one(at(10));
        window.add(at(12));
        window.add(at(11));
        assert_eq!(window.count, 3);
        assert_eq!(window.first, at(10));
        assert_eq!(window.last, at(12));

        let mut full = CountWindow {
            count: u32::MAX,
            ..window
        };
        assert!(full.is_full());
        full.add(at(13));
        assert_eq!(full.count, u32::MAX);
    }

    #[test]
    fn the_throttle_gives_a_window_at_once_and_then_at_most_once_each_interval() {
        let start = Instant::now();
        let after = |seconds| start + Duration::from_secs(seconds);
        let mut throttle = CountThrottle::new(Duration::from_secs(60));
        assert_eq!(throttle.due(start), None);

        assert_eq!(throttle.count(start, at(0)), Some(CountWindow::one(at(0))));
        assert_eq!(throttle.count(after(1), at(1)), None);
        assert_eq!(throttle.count(after(30), at(30)), None);
        assert_eq!(throttle.due(after(59)), None);
        assert_eq!(
            throttle.due(after(60)),
            Some(CountWindow {
                count: 2,
                first: at(1),
                last: at(30)
            })
        );
        // Nothing to record.
        assert_eq!(throttle.due(after(200)), None);
        // The last record was more than an interval ago.
        assert_eq!(
            throttle.count(after(201), at(201)),
            Some(CountWindow::one(at(201)))
        );
        assert_eq!(throttle.count(after(202), at(202)), None);
        assert_eq!(throttle.take(), Some(CountWindow::one(at(202))));
        assert_eq!(throttle.take(), None);
    }

    #[test]
    fn a_full_window_is_given_at_once_and_counting_goes_on() {
        let start = Instant::now();
        let mut throttle = CountThrottle::new(Duration::from_secs(60));
        assert!(throttle.count(start, at(0)).is_some());
        throttle.count(start, at(1));
        throttle.pending = throttle.pending.map(|window| CountWindow {
            count: u32::MAX,
            ..window
        });
        let full = throttle.count(start, at(2)).unwrap();
        assert_eq!(full.count, u32::MAX);
        assert_eq!(throttle.take(), Some(CountWindow::one(at(2))));
    }

    fn buffer(capacity: usize) -> (EventBuffer, Arc<Notify>) {
        let wake = Arc::new(Notify::new());
        (EventBuffer::new(capacity, Arc::clone(&wake)), wake)
    }

    #[tokio::test(start_paused = true)]
    async fn the_buffer_keeps_the_events_in_order_until_the_writer_removes_them() {
        let (events, wake) = buffer(10);
        assert_eq!(events.oldest(), None);
        let first = Instant::now();
        events.record(IngestEvent::now(EventKind::Started));
        // The first event wakes the writer.
        tokio::time::timeout(Duration::from_secs(1), wake.notified())
            .await
            .unwrap();
        tokio::time::advance(Duration::from_secs(5)).await;
        events.record(IngestEvent::now(EventKind::GateOpened));
        assert_eq!(events.oldest(), Some(first));
        let names: Vec<_> = events.pending().iter().map(|e| e.kind.name()).collect();
        assert_eq!(names, ["started", "gate_opened"]);
        events.remove(1);
        assert_eq!(events.len(), 1);
        events.remove(5);
        assert!(events.is_empty());
    }

    #[test]
    fn the_buffer_is_bounded_and_counts_what_it_does_not_keep() {
        let (events, _wake) = buffer(2);
        for _ in 0..5 {
            events.record(IngestEvent::now(EventKind::GateOpened));
        }
        assert_eq!(events.len(), 2);
        assert_eq!(events.not_kept(), 3);
        // The `stopped` event is always kept.
        events.record(IngestEvent::now(EventKind::Stopped { reason: "SIGTERM" }));
        assert_eq!(events.len(), 3);
        assert_eq!(events.pending()[2].kind.name(), "stopped");
    }

    #[test]
    fn the_buffer_makes_the_times_strictly_increasing_in_microseconds() {
        let (events, _wake) = buffer(10);
        let same = Timestamp::new(1_800_000_000, 5_000_100).unwrap();
        for kind in [
            EventKind::GateOpened,
            EventKind::GateOpened,
            EventKind::GateClosed { reason: "x" },
        ] {
            events.record(IngestEvent {
                occurred_at: same,
                kind,
            });
        }
        // A clock that went back.
        events.record(IngestEvent {
            occurred_at: at(1_700_000_000),
            kind: EventKind::GateOpened,
        });
        let micros: Vec<i64> = events
            .pending()
            .iter()
            .map(|event| event.occurred_at.as_microsecond())
            .collect();
        assert_eq!(
            micros,
            [
                1_800_000_000_005_000,
                1_800_000_000_005_001,
                1_800_000_000_005_002,
                1_800_000_000_005_003
            ]
        );
        // The first time stays unchanged, with its nanoseconds.
        assert_eq!(events.pending()[0].occurred_at, same);
    }

    #[test]
    fn the_log_writes_the_kind_the_reason_and_the_count() {
        let (capture, guard) = capture::install(ALLOWED_FIELDS);
        let (events, _wake) = buffer(1);
        events.record(IngestEvent::now(EventKind::GateFull(CountWindow::one(at(
            1,
        )))));
        events.record(IngestEvent::now(EventKind::GateClosed {
            reason: "child_not_running",
        }));
        // Recording writes no log line; the log task does.
        assert_eq!(capture.text(), "");
        events.log_pending();
        drop(guard);
        let lines: Vec<serde_json::Value> = capture
            .text()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines[0]["message"], "ingest_event");
        assert_eq!(lines[0]["kind"], "gate_full");
        assert_eq!(lines[0]["count"], 1);
        // The log keeps one event; the second one is counted.
        assert_eq!(lines[1]["message"], "events_not_logged");
        assert_eq!(lines[1]["count"], 1);
        assert_eq!(lines.len(), 2);
    }
}
