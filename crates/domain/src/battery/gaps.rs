//! Losses and restart markers from the `raw.ingest_event` rows (ADR 0013).
//!
//! The view shows only what the ingest process records itself:
//!
//! - A gap is a row that states that records or messages were lost:
//!   `records_dropped` (reason `queue_full` or `database_refused`) and
//!   `zmq_shortfall`, with the count and the receive times of the row.
//!   Every time here is on the ingest clock.
//! - A restart marker is the time of a `started` row: the ingest process
//!   started there. It states nothing about a loss.
//!
//! Each list holds the rows of the window, at most a limit, and says when
//! the limit cut it.

use jiff::Timestamp;

use super::ContractViolation;
use super::range::Window;

/// The largest number of gaps in a view.
pub const MAX_GAPS: usize = 500;

/// The largest number of restart markers in a view.
pub const MAX_RESTARTS: usize = 500;

/// The `reason` of a `records_dropped` event with a full queue.
pub const REASON_QUEUE_FULL: &str = "queue_full";

/// The `reason` of a `records_dropped` event that the database refused.
pub const REASON_DATABASE_REFUSED: &str = "database_refused";

/// The identifier of one ingest run (`run_id`, a UUID as 128 bits).
pub type RunId = u128;

/// The `kind` of an ingest event (`raw.ingest_event.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// `started`.
    Started,
    /// `stopped`.
    Stopped,
    /// `gate_opened`.
    GateOpened,
    /// `gate_closed`.
    GateClosed,
    /// `degraded_mode`.
    DegradedMode,
    /// `child_exited`.
    ChildExited,
    /// `records_dropped`.
    RecordsDropped,
    /// `zmq_shortfall`.
    ZmqShortfall,
    /// `gate_full`.
    GateFull,
}

impl EventKind {
    /// Every kind.
    pub const ALL: [Self; 9] = [
        Self::Started,
        Self::Stopped,
        Self::GateOpened,
        Self::GateClosed,
        Self::DegradedMode,
        Self::ChildExited,
        Self::RecordsDropped,
        Self::ZmqShortfall,
        Self::GateFull,
    ];

    /// The text of the `kind` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Stopped => "stopped",
            Self::GateOpened => "gate_opened",
            Self::GateClosed => "gate_closed",
            Self::DegradedMode => "degraded_mode",
            Self::ChildExited => "child_exited",
            Self::RecordsDropped => "records_dropped",
            Self::ZmqShortfall => "zmq_shortfall",
            Self::GateFull => "gate_full",
        }
    }

    /// The kind of a `kind` text.
    ///
    /// # Errors
    ///
    /// Returns [`ContractViolation::UnknownEventKind`] for another text.
    pub fn parse(text: &str) -> Result<Self, ContractViolation> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == text)
            .ok_or(ContractViolation::UnknownEventKind)
    }
}

/// One `raw.ingest_event` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    /// The run of the event.
    pub run_id: RunId,
    /// The time of the event.
    pub occurred_at: Timestamp,
    /// The kind.
    pub kind: EventKind,
    /// Static text, or `None`.
    pub reason: Option<String>,
    /// The count of a counted kind.
    pub dropped_count: Option<i64>,
    /// The first time of a counted kind.
    pub first_received_at: Option<Timestamp>,
    /// The last time of a counted kind.
    pub last_received_at: Option<Timestamp>,
}

impl EventRow {
    /// True for a row that states a loss: `records_dropped` with a known
    /// reason, or `zmq_shortfall`.
    #[must_use]
    pub fn is_loss(&self) -> bool {
        match self.kind {
            EventKind::RecordsDropped => matches!(
                self.reason.as_deref(),
                Some(REASON_QUEUE_FULL | REASON_DATABASE_REFUSED)
            ),
            EventKind::ZmqShortfall => true,
            _ => false,
        }
    }
}

/// The cause of a gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapKind {
    /// `records_dropped` with `queue_full`.
    QueueFull,
    /// `records_dropped` with `database_refused`.
    DatabaseRefused,
    /// `zmq_shortfall`.
    ZmqShortfall,
}

/// What a count counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountUnit {
    /// Records.
    Records,
    /// ZMQ messages.
    Messages,
}

/// The recorded count of a gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GapCount {
    /// At least 1.
    pub value: u32,
    /// What it counts.
    pub of: CountUnit,
}

/// One recorded loss: the receive times of the first and the last lost
/// item, `from <= to`, on the ingest clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    /// The cause.
    pub kind: GapKind,
    /// The first lost item.
    pub from: Timestamp,
    /// The last lost item.
    pub to: Timestamp,
    /// The recorded count.
    pub count: GapCount,
}

/// A list that a limit can cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limited<T> {
    /// The items.
    pub items: Vec<T>,
    /// True when more items exist than the limit.
    pub truncated: bool,
}

impl<T> Default for Limited<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            truncated: false,
        }
    }
}

impl<T> Limited<T> {
    /// The first `limit` items, and whether more exist.
    #[must_use]
    pub fn cut(mut items: Vec<T>, limit: usize) -> Self {
        let truncated = items.len() > limit;
        items.truncate(limit);
        Self { items, truncated }
    }
}

/// The ingest events of a request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EventSnapshot {
    /// The loss rows whose interval meets the window, in the order of
    /// [`loss_order`].
    pub losses: Limited<EventRow>,
    /// The `started` times in the window, in order.
    pub restarts: Limited<Timestamp>,
}

/// The order of the loss rows: by the first and the last receive time,
/// then by the time, run, and kind of the row (the key of the table), so
/// the cut at the limit does not depend on the input order.
#[must_use]
pub fn loss_order(
    row: &EventRow,
) -> (
    Option<Timestamp>,
    Option<Timestamp>,
    Timestamp,
    RunId,
    &'static str,
) {
    (
        row.first_received_at,
        row.last_received_at,
        row.occurred_at,
        row.run_id,
        row.kind.as_str(),
    )
}

/// The reference selection of the events of a request. The PostgreSQL
/// adapter selects the same rows with SQL.
///
/// - Losses: the loss rows ([`EventRow::is_loss`]) whose interval meets the
///   window: `last_received_at >= from` and `first_received_at < to`.
/// - Restarts: the `started` rows with `occurred_at` in the window.
#[must_use]
pub fn select_events(window: &Window, all: &[EventRow]) -> EventSnapshot {
    let mut losses: Vec<EventRow> = all
        .iter()
        .filter(|row| {
            row.is_loss()
                && row
                    .last_received_at
                    .is_some_and(|last| last >= window.from())
                && row
                    .first_received_at
                    .is_some_and(|first| first < window.to())
        })
        .cloned()
        .collect();
    losses.sort_by(|a, b| loss_order(a).cmp(&loss_order(b)));
    let mut restarts: Vec<Timestamp> = all
        .iter()
        .filter(|row| row.kind == EventKind::Started && window.contains(row.occurred_at))
        .map(|row| row.occurred_at)
        .collect();
    restarts.sort();
    EventSnapshot {
        losses: Limited::cut(losses, MAX_GAPS),
        restarts: Limited::cut(restarts, MAX_RESTARTS),
    }
}

/// The gap of one loss row.
///
/// # Errors
///
/// Returns a [`ContractViolation`] for a row that breaks the table
/// contract: a row that is no loss, a count or a time that is missing, a
/// count below 1 or above `u32::MAX`, or `first > last`.
pub fn gap(row: &EventRow) -> Result<Gap, ContractViolation> {
    let (kind, of) = match (row.kind, row.reason.as_deref()) {
        (EventKind::RecordsDropped, Some(REASON_QUEUE_FULL)) => {
            (GapKind::QueueFull, CountUnit::Records)
        }
        (EventKind::RecordsDropped, Some(REASON_DATABASE_REFUSED)) => {
            (GapKind::DatabaseRefused, CountUnit::Records)
        }
        (EventKind::ZmqShortfall, _) => (GapKind::ZmqShortfall, CountUnit::Messages),
        _ => return Err(ContractViolation::BadEventRow),
    };
    let (Some(count), Some(from), Some(to)) = (
        row.dropped_count,
        row.first_received_at,
        row.last_received_at,
    ) else {
        return Err(ContractViolation::BadEventRow);
    };
    let value = u32::try_from(count).map_err(|_| ContractViolation::CountTooLarge)?;
    if value == 0 || from > to {
        return Err(ContractViolation::BadEventRow);
    }
    Ok(Gap {
        kind,
        from,
        to,
        count: GapCount { value, of },
    })
}

/// The gaps of the selected loss rows, in their order.
///
/// # Errors
///
/// Returns the [`ContractViolation`] of the first row that breaks the
/// table contract ([`gap`]).
pub fn derive_gaps(losses: &Limited<EventRow>) -> Result<Limited<Gap>, ContractViolation> {
    Ok(Limited {
        items: losses.items.iter().map(gap).collect::<Result<_, _>>()?,
        truncated: losses.truncated,
    })
}

#[cfg(test)]
mod tests;
