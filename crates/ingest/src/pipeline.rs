//! The delivery path of ADR 0001: the ZMQ reader, the bounded queue, the
//! writer, and the shortfall check.
//!
//! ```text
//! fleet-telemetry --ZMQ PUB--> reader --> queue --> writer --> PostgreSQL
//!        |                       |                    |
//!        +--/metrics--> shortfall check    events (EventBuffer)
//! ```
//!
//! | Part | Task | What it owns |
//! | ---- | ---- | ------------ |
//! | [`reader`] | the reader task, watchdog task `reader` (30 s) | the SUB socket, gate condition 2 (connected and settled), the drops of a full queue, the received counts of the shortfall check |
//! | [`queue`] | none, shared | the records until their rows commit, gate condition 4 (the high and the low mark) |
//! | [`writer`] | the writer task, watchdog task `writer` (120 s) | the start-up check and degraded mode (gate condition 3), the flushes, the retry cadence, the final flush |
//! | [`shortfall`] | the shortfall task | the reads of `dispatch_total` and the `zmq_shortfall` events |
//! | [`conditions`] | none, shared | the one [`crate::readiness::Pipeline`] value that the three conditions give |
//!
//! The events of the whole process wait in the
//! [`EventBuffer`](crate::events::EventBuffer) and go to the database with
//! the flushes of the writer.
//!
//! # Bounds
//!
//! Each bound of [`Limits`] has the value of ADR 0001 in
//! [`Limits::PRODUCTION`]. Only the end-to-end tests make some of them
//! smaller ([`crate::config::TestOverrides`]).
//!
//! | Bound | Value | Source |
//! | ----- | ----- | ------ |
//! | Queue | 64 MiB of payload bytes and decoded rows | ADR 0001, "Delivery path", step 2 |
//! | Gate marks | closes at 75 %, opens below 25 % after a successful write, or when the queue is empty | ADR 0001, gate condition 4 |
//! | Flush | oldest item 900 s old, 5,000 signal rows, the high mark, or the shutdown | ADR 0001, step 4 |
//! | Settle | 250 ms after the connect and the subscription of a SUB socket | ADR 0001, gate condition 2 |
//! | Socket rebuild | drain the old socket to its end, at most 1 s (quiet 200 ms); after a lost peer, wait 0, then 100 ms doubling to 5 s | ADR 0001, "SUB socket" |
//! | Degraded mode | 120 s of no answer | ADR 0001, gate condition 3 |
//! | Retry | 1 s, doubling to 5 minutes, with jitter; 60 minutes after three equal answered failures | ADR 0001, step 5 |
//! | Shortfall check and stall rule | each 15 s, one HTTP read of at most 1 MiB within 0.75 s (the shutdown step bound of 1 s minus the step margin of 250 ms) | ADR 0001, "ZMQ shortfall check" and "SUB socket" |
//! | Events | 10,000 events wait at most | this change |

pub mod cadence;
pub mod conditions;
pub mod metrics;
pub mod queue;
pub mod reader;
pub mod shortfall;
pub mod writer;

use std::time::Duration;

use crate::storage::MAX_SIGNAL_ROWS_PER_TRANSACTION;

/// The bounds of the pipeline (module documentation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The queue bound in bytes: payload bytes and decoded rows.
    pub queue_bytes: usize,
    /// The number of waiting signal rows that starts a flush.
    pub flush_rows: usize,
    /// The time after the connect and the subscription of a SUB socket
    /// before the gate can open.
    pub settle: Duration,
    /// The time of no answer before degraded mode.
    pub degraded_after: Duration,
    /// The first retry delay.
    pub retry_first: Duration,
    /// The longest retry delay of the doubling cadence.
    pub retry_max: Duration,
    /// The number of equal answered failures in a row that slows the
    /// cadence.
    pub answered_repeats: u32,
    /// The retry delay after `answered_repeats` equal answered failures.
    pub answered_delay: Duration,
    /// The period of the shortfall check.
    pub shortfall_period: Duration,
    /// The largest body of the metrics response.
    pub metrics_body_bytes: usize,
    /// The quiet time that ends the drain of the SUB socket at the
    /// shutdown.
    pub drain_quiet: Duration,
    /// The time for one connect attempt of the SUB socket. A new attempt
    /// follows at once.
    pub connect_attempt: Duration,
    /// The longest drain of an old SUB socket before the reader replaces
    /// it.
    pub rebuild_drain: Duration,
    /// The quiet time that ends the drain of an old SUB socket early.
    pub rebuild_quiet: Duration,
    /// The first wait before a new SUB socket after a lost peer.
    pub rebuild_backoff_first: Duration,
    /// The longest wait before a new SUB socket after a lost peer.
    pub rebuild_backoff_max: Duration,
    /// The most events that wait for the writer.
    pub max_events: usize,
    /// The shortest time between two `records_dropped` events of the
    /// reader.
    pub drop_event_interval: Duration,
}

impl Limits {
    /// The values of ADR 0001.
    pub const PRODUCTION: Self = Self {
        queue_bytes: 64 * 1024 * 1024,
        flush_rows: MAX_SIGNAL_ROWS_PER_TRANSACTION,
        settle: Duration::from_millis(250),
        degraded_after: Duration::from_secs(120),
        retry_first: Duration::from_secs(1),
        retry_max: Duration::from_mins(5),
        answered_repeats: 3,
        answered_delay: Duration::from_mins(60),
        shortfall_period: Duration::from_secs(15),
        metrics_body_bytes: 1024 * 1024,
        drain_quiet: Duration::from_secs(1),
        connect_attempt: Duration::from_secs(5),
        rebuild_drain: Duration::from_secs(1),
        rebuild_quiet: Duration::from_millis(200),
        rebuild_backoff_first: Duration::from_millis(100),
        rebuild_backoff_max: Duration::from_secs(5),
        max_events: 10_000,
        drop_event_interval: Duration::from_secs(60),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_production_limits_are_those_of_adr_0001() {
        let limits = Limits::PRODUCTION;
        assert_eq!(limits.queue_bytes, 67_108_864);
        assert_eq!(limits.flush_rows, 5_000);
        assert_eq!(limits.settle, Duration::from_millis(250));
        assert_eq!(limits.degraded_after, Duration::from_secs(120));
        assert_eq!(limits.retry_first, Duration::from_secs(1));
        assert_eq!(limits.retry_max, Duration::from_secs(300));
        assert_eq!(limits.answered_repeats, 3);
        assert_eq!(limits.answered_delay, Duration::from_secs(3_600));
        assert_eq!(limits.shortfall_period, Duration::from_secs(15));
        assert_eq!(limits.drain_quiet, Duration::from_secs(1));
        // A connect attempt beats its watchdog: it is far below the 30 s of
        // the reader.
        assert!(limits.connect_attempt < crate::watchdog::READER.limit);
        // The stall rule decides within two periods; that bound of lost
        // records stays far below the vehicle buffer of 2,500 s (ADR 0001,
        // "SUB socket").
        assert!(limits.shortfall_period * 2 <= Duration::from_secs(30));
    }
}
