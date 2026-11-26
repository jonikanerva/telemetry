//! The `ingest_event` records of ADR 0002 and the sink that takes them.
//!
//! ADR 0002 stores each event of the process in `raw.ingest_event`. The
//! gate, the readiness input, and the child supervisor make these events.
//! ADR 0001 puts the events into the memory queue with the records, so the
//! pipeline change implements [`EventSink`] on its queue. Until then, the
//! process uses [`LogSink`], which writes each event to the log and stores
//! nothing.
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
//! state; the watchdog then ends the process.

use jiff::Timestamp;
use tracing::info;

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
    /// The gate opened in degraded mode (ADR 0001, gate condition 3).
    DegradedMode,
    /// The child process ended, or it could not start.
    ChildExited {
        /// How the child ended.
        exit: ChildExit,
    },
    /// The reader dropped messages because the queue was full.
    RecordsDropped(CountWindow),
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
            Self::RecordsDropped(_) => "records_dropped",
            Self::ZmqShortfall(_) => "zmq_shortfall",
            Self::GateFull(_) => "gate_full",
        }
    }

    /// The value of the `reason` column, if the kind has one. The text holds
    /// static words and numbers only.
    #[must_use]
    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Stopped { reason } | Self::GateClosed { reason } => Some((*reason).to_owned()),
            Self::ChildExited { exit } => Some(exit.to_string()),
            _ => None,
        }
    }

    /// The count and the time window, if the kind has them.
    #[must_use]
    pub const fn window(&self) -> Option<&CountWindow> {
        match self {
            Self::RecordsDropped(window) | Self::ZmqShortfall(window) | Self::GateFull(window) => {
                Some(window)
            }
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
}

/// The receiver of the `ingest_event` records.
pub trait EventSink: Send + Sync {
    /// Takes one event. Returns at once, does not block, and does not call
    /// into [`crate::readiness::Readiness`] (module documentation).
    fn record(&self, event: IngestEvent);
}

/// The sink of this phase: it writes each event to the log as the event
/// `ingest_event` and keeps nothing. The pipeline change replaces it with the
/// queue sink.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogSink;

impl EventSink for LogSink {
    fn record(&self, event: IngestEvent) {
        let kind = event.kind.name();
        let reason = event.kind.reason();
        let count = event.kind.window().map(|window| window.count);
        info!(kind, reason = reason.as_deref(), count, "ingest_event");
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
            (EventKind::RecordsDropped(window), "records_dropped"),
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
    fn child_exit_reasons_are_static_words_and_numbers() {
        let reason = |exit| EventKind::ChildExited { exit }.reason().unwrap();
        assert_eq!(reason(ChildExit::Code(3)), "exit code 3");
        assert_eq!(reason(ChildExit::Signal(9)), "signal 9");
        assert_eq!(reason(ChildExit::Unknown), "unknown status");
        assert_eq!(reason(ChildExit::SpawnFailed), "spawn failed");
        assert_eq!(reason(ChildExit::WaitFailed), "wait failed");
        assert_eq!(EventKind::Started.reason(), None);
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
        full.add(at(13));
        assert_eq!(full.count, u32::MAX);
    }

    #[test]
    fn the_log_sink_writes_the_kind_the_reason_and_the_count() {
        let (capture, guard) = capture::install(ALLOWED_FIELDS);
        LogSink.record(IngestEvent::now(EventKind::GateFull(CountWindow::one(at(
            1,
        )))));
        LogSink.record(IngestEvent::now(EventKind::GateClosed {
            reason: "child_not_running",
        }));
        drop(guard);
        let lines: Vec<serde_json::Value> = capture
            .text()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines[0]["message"], "ingest_event");
        assert_eq!(lines[0]["kind"], "gate_full");
        assert_eq!(lines[0]["count"], 1);
        assert_eq!(lines[1]["kind"], "gate_closed");
        assert_eq!(lines[1]["reason"], "child_not_running");
    }
}
