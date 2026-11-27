//! The ZMQ shortfall check of ADR 0001.
//!
//! PUB drops messages without notice when its send queue for the
//! subscriber is full. The check compares the `dispatch_total` counter of
//! `fleet-telemetry` ([`super::metrics`]) with the messages that the reader
//! received for the same record types.
//!
//! - **Received count.** The reader counts each message by its raw topic
//!   (`tesla_telemetry_V`, `_alerts`, `_errors`) before the decode and
//!   before the decision to queue or drop it ([`Received`]). A message
//!   that the full queue drops was received; its `records_dropped` event
//!   covers it, so it is not a shortfall too.
//! - **Generation.** The counter of `fleet-telemetry` starts at 0 with
//!   each child process. The reader starts a new generation of its counts
//!   with its first connected socket after a child stop. A new socket to
//!   the same child keeps the counts, because the counter of the child
//!   keeps its value too.
//! - **Gap.** The gap of one read is the sum over the three record types of
//!   `dispatched - received`, each at least 0. The reader count is read
//!   after the HTTP response, so a message is never counted as received
//!   before it is counted as dispatched. The counter grows after
//!   `Dispatch` returns, so one read can count messages that are still in
//!   flight.
//! - **Stall rule.** Each periodic read also feeds [`Liveness`], which
//!   asks the reader to replace a SUB socket that receives nothing while
//!   the child dispatches.
//! - **Periodic read.** Each [`super::Limits::shortfall_period`] (15 s).
//!   The check reports a shortfall only when two reads in a row both show a
//!   gap. The confirmed gap is the smaller of the two. The gap is
//!   cumulative for the generation, so the check reports only the part of
//!   the confirmed gap that it has not reported before, as a
//!   `zmq_shortfall` event. The time window of the event starts at the
//!   last read that showed no new gap, and ends at the read that confirms
//!   the gap. A failed read breaks the row of two reads.
//! - **Final read.** The shutdown sequence makes one read after its drain
//!   step (ADR 0001, step 3). No message is in flight then, so one read is
//!   enough, and every unreported part of its gap is reported.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use tokio::sync::{oneshot, watch};
use tracing::{info, warn};

use super::conditions::Conditions;
use super::metrics::{self, Dispatched};
use crate::events::{CountWindow, EventKind, EventSink, IngestEvent};
use crate::record::TOPIC_PREFIX;

/// The received counts of one generation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// The generation: one for each child process that the SUB socket
    /// connected to.
    pub generation: u64,
    /// The SUB socket of the reader: one for each connected socket, also
    /// within one generation ([`super::reader`]).
    pub socket: u64,
    /// Messages of topic `tesla_telemetry_V`.
    pub vehicle_data: u64,
    /// Messages of topic `tesla_telemetry_alerts`.
    pub alerts: u64,
    /// Messages of topic `tesla_telemetry_errors`.
    pub errors: u64,
}

/// The received counts that the reader writes and the check reads. Share
/// it with [`Arc`].
#[derive(Debug, Default)]
pub struct Received {
    counts: Mutex<Counts>,
}

impl Received {
    fn lock(&self) -> MutexGuard<'_, Counts> {
        self.counts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Counts one received message by its raw topic.
    pub fn count(&self, topic: &[u8]) {
        let Some(name) = topic.strip_prefix(TOPIC_PREFIX.as_bytes()) else {
            return;
        };
        let mut counts = self.lock();
        match name {
            b"V" => counts.vehicle_data += 1,
            b"alerts" => counts.alerts += 1,
            b"errors" => counts.errors += 1,
            _ => {}
        }
    }

    /// A new SUB socket of the reader is connected and subscribed. Returns
    /// its number. `new_child`: the socket is the first one to a new child
    /// process, so a new generation starts with the counts at 0.
    pub fn connected(&self, new_child: bool) -> u64 {
        let mut counts = self.lock();
        if new_child {
            *counts = Counts {
                generation: counts.generation.wrapping_add(1),
                socket: counts.socket,
                ..Counts::default()
            };
        }
        counts.socket = counts.socket.wrapping_add(1);
        counts.socket
    }

    /// The counts now.
    #[must_use]
    pub fn snapshot(&self) -> Counts {
        *self.lock()
    }
}

/// The gap of one read: for each record type `dispatched - received`, at
/// least 0, summed.
#[must_use]
pub fn gap(dispatched: Dispatched, received: Counts) -> u64 {
    dispatched
        .vehicle_data
        .saturating_sub(received.vehicle_data)
        .saturating_add(dispatched.alerts.saturating_sub(received.alerts))
        .saturating_add(dispatched.errors.saturating_sub(received.errors))
}

/// The state of the check (module documentation).
#[derive(Debug, Clone)]
pub struct Check {
    generation: Option<u64>,
    previous_gap: Option<u64>,
    reported: u64,
    quiet_since: Timestamp,
    last_read: Timestamp,
}

impl Check {
    /// A check that started at `now`.
    #[must_use]
    pub const fn new(now: Timestamp) -> Self {
        Self {
            generation: None,
            previous_gap: None,
            reported: 0,
            quiet_since: now,
            last_read: now,
        }
    }

    fn enter(&mut self, generation: u64) {
        if self.generation != Some(generation) {
            self.generation = Some(generation);
            self.previous_gap = None;
            self.reported = 0;
            self.quiet_since = self.last_read;
        }
    }

    /// A periodic read at `now`. Returns the window of a new shortfall.
    pub fn periodic(
        &mut self,
        dispatched: Dispatched,
        received: Counts,
        now: Timestamp,
    ) -> Option<CountWindow> {
        self.enter(received.generation);
        let gap = gap(dispatched, received);
        let confirmed = self.previous_gap.map(|previous| previous.min(gap));
        self.previous_gap = Some(gap);
        self.last_read = now;
        let window = confirmed.and_then(|confirmed| self.report(confirmed, now));
        if window.is_none() && gap <= self.reported {
            self.quiet_since = now;
        }
        window
    }

    /// The final read at `now`: one read is enough.
    pub fn final_read(
        &mut self,
        dispatched: Dispatched,
        received: Counts,
        now: Timestamp,
    ) -> Option<CountWindow> {
        self.enter(received.generation);
        self.last_read = now;
        self.report(gap(dispatched, received), now)
    }

    /// A read failed: the next read cannot confirm a gap.
    pub const fn read_failed(&mut self) {
        self.previous_gap = None;
    }

    fn report(&mut self, confirmed: u64, now: Timestamp) -> Option<CountWindow> {
        let new = confirmed
            .checked_sub(self.reported)
            .filter(|new| *new > 0)?;
        self.reported = confirmed;
        let window = CountWindow {
            count: u32::try_from(new).unwrap_or(u32::MAX),
            first: self.quiet_since.min(now),
            last: now,
        };
        self.quiet_since = now;
        Some(window)
    }
}

/// The sums of the three record types at one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Point {
    dispatched: u64,
    received: u64,
}

impl Point {
    const ZERO: Self = Self {
        dispatched: 0,
        received: 0,
    };

    const fn of(dispatched: Dispatched, received: Counts) -> Self {
        Self {
            dispatched: dispatched
                .vehicle_data
                .saturating_add(dispatched.alerts)
                .saturating_add(dispatched.errors),
            received: received
                .vehicle_data
                .saturating_add(received.alerts)
                .saturating_add(received.errors),
        }
    }
}

/// The stall rule of the SUB socket (ADR 0001, "SUB socket"): a liveness
/// rule that needs no event of the `zeromq` crate.
///
/// - **Rule.** Two successful reads `m` and `n` of one socket, with failed
///   reads between them or not. When the child dispatched records between
///   the response of `m` and the response of `n`, and the reader received
///   no record between the snapshot before the request of `m` and a
///   snapshot [`super::Limits::stall_grace`] (2 s) after the response of
///   `n`, the socket is stalled. A live subscription receives each record
///   that the child dispatched before the response of `n` within
///   milliseconds; the grace covers a slow turn of the reader. A drop at
///   the libzmq high water mark drops the newest records, and the queued
///   older ones still arrive. An idle child (no new dispatch) never gives a
///   stall.
/// - **Received count before the request.** A record that the reader
///   counts after the request of `m` and the child counts after the
///   response of `m` would otherwise look like a record of an earlier
///   read.
/// - **Start.** For a new generation, the first point is zero: the counter
///   of a new child and the received counts both start at 0, and no record
///   passes the closed gate before the socket settles. A new socket of the
///   same generation starts with no point, so records that the child
///   dispatched before the new subscription do not count against it. A
///   counter that goes down (a read of another child) starts again. A
///   failed read keeps the last point, so failed reads delay the decision
///   but cannot prevent it.
/// - **Bound.** ADR 0001, "SUB socket", "Detection bound", derives the
///   worst case; the tests pin it.
/// - **Repeated stalls.** The rule asks again at each stalled read; the
///   reader bounds the rate of new sockets ([`super::reader`]).
#[derive(Debug, Clone, Default)]
pub struct Liveness {
    key: Option<(u64, u64)>,
    last: Option<Point>,
}

impl Liveness {
    /// A successful periodic read: `dispatched` from the response,
    /// `before` from the snapshot before the request, and `after` from the
    /// snapshot one grace after the response. Returns the number of the
    /// stalled socket.
    pub fn observe(
        &mut self,
        dispatched: Dispatched,
        before: Counts,
        after: Counts,
    ) -> Option<u64> {
        let point = Point::of(dispatched, before);
        let key = (before.generation, before.socket);
        if self.key != Some(key) {
            let new_generation = self
                .key
                .is_none_or(|(generation, _)| generation != before.generation);
            self.last = new_generation.then_some(Point::ZERO);
            self.key = Some(key);
        }
        let same_socket = (after.generation, after.socket) == key;
        let received_after = Point::of(dispatched, after).received;
        let stalled = self.last.is_some_and(|last| {
            same_socket && point.dispatched > last.dispatched && received_after == last.received
        });
        self.last = Some(point);
        stalled.then_some(key.1)
    }
}

/// The count of failed metrics reads in a row, for gate condition 2
/// (ADR 0001, "SUB socket", "Unreadable metrics").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readable {
    failures: u32,
    limit: u32,
}

impl Readable {
    /// No failed read yet. `limit` failed reads in a row make the metrics
    /// unreadable.
    #[must_use]
    pub const fn new(limit: u32) -> Self {
        Self { failures: 0, limit }
    }

    /// A read failed. Returns true at the change to unreadable.
    pub const fn failed(&mut self) -> bool {
        self.failures = self.failures.saturating_add(1);
        self.failures == self.limit
    }

    /// A read succeeded. Returns true at the change to readable.
    pub const fn succeeded(&mut self) -> bool {
        let was = self.is_unreadable();
        self.failures = 0;
        was
    }

    /// True after `limit` failed reads in a row.
    #[must_use]
    pub const fn is_unreadable(&self) -> bool {
        self.failures >= self.limit
    }
}

/// The shortfall task: reads the metrics each period until the final read
/// is requested, then makes the final read and ends.
pub struct Shortfall {
    /// The loopback port of the metrics.
    pub port: u16,
    /// The period of the reads.
    pub period: Duration,
    /// The largest body of a response.
    pub max_body: usize,
    /// The deadline of one read.
    pub deadline: Duration,
    /// The wait after a response before the second snapshot of the stall
    /// rule.
    pub grace: Duration,
    /// The failed reads in a row that make the metrics unreadable.
    pub unreadable_after: u32,
    /// The period of the reads while the metrics are unreadable.
    pub retry: Duration,
    /// The counts of the reader.
    pub received: Arc<Received>,
    /// The sink of the `zmq_shortfall` events.
    pub sink: Arc<dyn EventSink>,
    /// The number of the socket that the stall rule found stalled. The
    /// reader replaces that socket ([`super::reader`]).
    pub stalled: watch::Sender<Option<u64>>,
    /// The owner of gate condition 2, for unreadable metrics.
    pub conditions: Arc<Conditions>,
}

impl Shortfall {
    /// Runs the task. `final_read` asks for the final read.
    pub async fn run(self, mut final_read: oneshot::Receiver<()>) {
        let mut check = Check::new(Timestamp::now());
        let mut liveness = Liveness::default();
        let mut readable = Readable::new(self.unreadable_after);
        let mut next = tokio::time::Instant::now() + self.period;
        // A dropped sender also asks for the end, without a read.
        let requested = loop {
            tokio::select! {
                () = tokio::time::sleep_until(next) => {
                    // The stall rule needs the received count before the
                    // request ([`Liveness`]).
                    let before = self.received.snapshot();
                    // The request of the final read stops a periodic read
                    // and its grace, so that the final read gets its whole
                    // step bound.
                    let read = metrics::read(self.port, self.max_body, self.deadline);
                    tokio::pin!(read);
                    let read = tokio::select! {
                        read = &mut read => read,
                        requested = &mut final_read => break requested.is_ok(),
                    };
                    let dispatched = read.as_ref().ok().copied();
                    self.observe(&mut check, &mut readable, read, false);
                    if let Some(dispatched) = dispatched {
                        tokio::select! {
                            () = tokio::time::sleep(self.grace.min(self.period)) => {}
                            requested = &mut final_read => break requested.is_ok(),
                        }
                        let after = self.received.snapshot();
                        if let Some(socket) = liveness.observe(dispatched, before, after) {
                            warn!("subscriber_stalled");
                            self.stalled.send_replace(Some(socket));
                        }
                    }
                    let now = tokio::time::Instant::now();
                    next = if readable.is_unreadable() {
                        now + self.retry.min(self.period)
                    } else {
                        (next + self.period).max(now)
                    };
                }
                requested = &mut final_read => break requested.is_ok(),
            }
        };
        if requested {
            let read = metrics::read(self.port, self.max_body, self.deadline).await;
            self.observe(&mut check, &mut readable, read, true);
        }
    }

    fn observe(
        &self,
        check: &mut Check,
        readable: &mut Readable,
        read: Result<Dispatched, metrics::ReadError>,
        last: bool,
    ) {
        match read {
            Ok(dispatched) => {
                if readable.succeeded() {
                    info!("metrics_readable");
                    self.conditions.set_metrics_readable(true);
                }
                // After the response: see the module documentation.
                let received = self.received.snapshot();
                let now = Timestamp::now();
                let window = if last {
                    check.final_read(dispatched, received, now)
                } else {
                    check.periodic(dispatched, received, now)
                };
                if last {
                    info!(count = gap(dispatched, received), "shortfall_final_read");
                }
                if let Some(window) = window {
                    warn!(count = window.count, "zmq_shortfall");
                    self.sink
                        .record(IngestEvent::now(EventKind::ZmqShortfall(window)));
                }
            }
            Err(error) => {
                check.read_failed();
                warn!(reason = error.as_str(), "shortfall_read_failed");
                if readable.failed() && !last {
                    warn!("metrics_unreadable");
                    self.conditions.set_metrics_readable(false);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    fn dispatched(vehicle_data: u64, alerts: u64, errors: u64) -> Dispatched {
        Dispatched {
            vehicle_data,
            alerts,
            errors,
        }
    }

    fn received(generation: u64, vehicle_data: u64, alerts: u64, errors: u64) -> Counts {
        Counts {
            generation,
            socket: 0,
            vehicle_data,
            alerts,
            errors,
        }
    }

    #[test]
    fn the_reader_counts_the_three_topics_by_generation() {
        let counts = Received::default();
        for topic in [
            &b"tesla_telemetry_V"[..],
            b"tesla_telemetry_V",
            b"tesla_telemetry_alerts",
            b"tesla_telemetry_errors",
            b"tesla_telemetry_connectivity",
            b"tesla_telemetry_Vx",
            b"other_V",
        ] {
            counts.count(topic);
        }
        assert_eq!(counts.snapshot(), received(0, 2, 1, 1));
        // The first socket to a new child: a new generation.
        assert_eq!(counts.connected(true), 1);
        assert_eq!(
            counts.snapshot(),
            Counts {
                socket: 1,
                ..received(1, 0, 0, 0)
            }
        );
        // A new socket to the same child keeps the counts.
        counts.count(b"tesla_telemetry_V");
        assert_eq!(counts.connected(false), 2);
        assert_eq!(
            counts.snapshot(),
            Counts {
                socket: 2,
                ..received(1, 1, 0, 0)
            }
        );
    }

    /// The counts of socket `socket` in generation `generation`, with
    /// `vehicle_data` received records.
    fn on(generation: u64, socket: u64, vehicle_data: u64) -> Counts {
        Counts {
            socket,
            ..received(generation, vehicle_data, 0, 0)
        }
    }

    /// One successful read of the stall rule on socket `socket` of
    /// generation `generation`: the counter, the received count before the
    /// request, and the received count one grace after the response.
    fn read(
        rule: &mut Liveness,
        (generation, socket): (u64, u64),
        counter: u64,
        before: u64,
        after: u64,
    ) -> Option<u64> {
        rule.observe(
            dispatched(counter, 0, 0),
            on(generation, socket, before),
            on(generation, socket, after),
        )
    }

    const SOCKET_1: (u64, u64) = (1, 1);

    #[test]
    fn a_socket_that_receives_nothing_while_the_child_dispatches_is_stalled() {
        // A new generation starts at zero, so the first read can decide.
        let mut rule = Liveness::default();
        assert_eq!(read(&mut rule, SOCKET_1, 3, 0, 0), Some(1));
        // The same reads with the records received within the grace.
        let mut rule = Liveness::default();
        assert_eq!(read(&mut rule, SOCKET_1, 3, 0, 3), None);
    }

    #[test]
    fn an_idle_child_or_a_live_socket_is_never_stalled() {
        let mut rule = Liveness::default();
        // Idle: the counter does not grow.
        for _ in 0..10 {
            assert_eq!(read(&mut rule, SOCKET_1, 0, 0, 0), None);
        }
        // Live: the records of each read arrive within the grace.
        let mut rule = Liveness::default();
        for step in 1..10 {
            assert_eq!(
                read(&mut rule, SOCKET_1, step * 10, (step - 1) * 10, step * 10),
                None
            );
        }
        // A drop at the high water mark: the newest records are lost, and
        // the queued older ones still arrive.
        let mut rule = Liveness::default();
        assert_eq!(read(&mut rule, SOCKET_1, 1_000, 400, 1_000), None);
        assert_eq!(read(&mut rule, SOCKET_1, 1_500, 1_000, 1_000), None);
        assert_eq!(read(&mut rule, SOCKET_1, 1_500, 1_000, 1_000), None);
    }

    #[test]
    fn a_new_socket_of_the_same_child_starts_with_its_own_reads() {
        let mut rule = Liveness::default();
        assert_eq!(read(&mut rule, SOCKET_1, 3, 0, 0), Some(1));
        // Socket 2 of the same child. The records that the child
        // dispatched before the new subscription (3 to 8) do not count
        // against socket 2: its first read is its first point.
        assert_eq!(read(&mut rule, (1, 2), 8, 0, 0), None);
        assert_eq!(read(&mut rule, (1, 2), 8, 0, 0), None);
        // A stall of socket 2 itself, while the child goes on dispatching:
        // asked at each stalled read.
        assert_eq!(read(&mut rule, (1, 2), 9, 0, 0), Some(2));
        assert_eq!(read(&mut rule, (1, 2), 10, 0, 0), Some(2));
    }

    #[test]
    fn a_record_that_arrives_during_the_request_is_no_stall() {
        // The record arrives after the snapshot before the request of the
        // first read, and the child counts it after that response. The
        // second read sees the counter grow, and the record is already in
        // the snapshot of the first read after its grace.
        let mut rule = Liveness::default();
        assert_eq!(read(&mut rule, SOCKET_1, 5, 5, 6), None);
        assert_eq!(read(&mut rule, SOCKET_1, 6, 6, 6), None);
        assert_eq!(read(&mut rule, SOCKET_1, 6, 6, 6), None);
    }

    #[test]
    fn the_microsecond_window_of_the_counter_is_a_known_false_stall() {
        // ADR 0001, "SUB socket", note N11: the reader counts a record
        // before `fleet-telemetry` adds it to `dispatch_total`. When the
        // snapshot before the request and the response both fall into that
        // gap of microseconds, and no other record follows, the rule asks
        // for a new socket. This test pins the known limit.
        let mut rule = Liveness::default();
        assert_eq!(read(&mut rule, SOCKET_1, 5, 6, 6), None);
        assert_eq!(read(&mut rule, SOCKET_1, 6, 6, 6), Some(1));
    }

    #[test]
    fn a_failed_read_keeps_the_last_point_and_a_lower_counter_starts_again() {
        let mut rule = Liveness::default();
        assert_eq!(read(&mut rule, SOCKET_1, 0, 0, 0), None);
        assert_eq!(read(&mut rule, SOCKET_1, 2, 0, 2), None);
        // A failed read gives no call. The next read compares with the
        // last successful one, so failed reads delay the decision but
        // cannot prevent it.
        assert_eq!(read(&mut rule, SOCKET_1, 5, 2, 2), None);
        assert_eq!(read(&mut rule, SOCKET_1, 7, 2, 2), Some(1));
        // A counter that goes down is another child: no decision on it.
        let mut rule = Liveness::default();
        assert_eq!(read(&mut rule, SOCKET_1, 20, 20, 20), None);
        assert_eq!(read(&mut rule, SOCKET_1, 10, 20, 20), None);
        assert_eq!(read(&mut rule, SOCKET_1, 12, 20, 20), Some(1));
    }

    /// The timeline of the stall rule in milliseconds: reads each
    /// `PERIOD`, the second snapshot `GRACE` after each read, and one
    /// record dispatched each second at `offset + k * 1000`. Records that
    /// the child dispatches up to `last_received` arrive at once; later
    /// ones are lost. The child dispatches nothing in `idle`. Reads with an
    /// index in `failed` fail. Returns the time of the first stall request.
    fn first_request(
        offset: u64,
        last_received: u64,
        idle: std::ops::Range<u64>,
        failed: &[u64],
    ) -> Option<u64> {
        const PERIOD: u64 = 15_000;
        const GRACE: u64 = 2_000;
        let records = |until: u64| -> u64 {
            let count = (0..=until / 1_000)
                .map(|k| offset + k * 1_000)
                .filter(|&at| at <= until && !idle.contains(&at))
                .count();
            u64::try_from(count).unwrap()
        };
        let received = |at: u64| records(at.min(last_received));
        let mut rule = Liveness::default();
        for index in 0..40 {
            if failed.contains(&index) {
                continue;
            }
            let at = index * PERIOD;
            if read(
                &mut rule,
                SOCKET_1,
                records(at),
                received(at),
                received(at + GRACE),
            )
            .is_some()
            {
                return Some(at + GRACE);
            }
        }
        None
    }

    #[test]
    fn the_worst_case_timeline_meets_the_bound_of_adr_0001() {
        // ADR 0001, "SUB socket", "Detection bound": records can be lost
        // for less than (2 + f) * P + G + D, with f failed reads, P = 15 s,
        // G = 2 s, and D = the read deadline (0 in this model). The worst
        // case: the stall starts just after the snapshot of a read. Here
        // the last received record is dispatched at 30.001 s, and the
        // first lost one at 31.001 s.
        let first_lost = 31_001;
        let decided = first_request(1, 30_001, 0..0, &[]).unwrap();
        assert_eq!(decided, 62_000);
        assert!(decided - first_lost < 2 * 15_000 + 2_000);
        // One failed read (at 45 s): one more period.
        let decided = first_request(1, 30_001, 0..0, &[3]).unwrap();
        assert_eq!(decided, 77_000);
        assert!(decided - first_lost < 3 * 15_000 + 2_000);
        // Two failed reads, not in a row (at 45 s and 75 s): two more
        // periods. Two failed reads in a row close the gate instead
        // ("Unreadable metrics").
        let decided = first_request(1, 30_001, 0..0, &[3, 5]).unwrap();
        assert_eq!(decided, 92_000);
        assert!(decided - first_lost < 4 * 15_000 + 2_000);
        // A socket that is dead from its start in a new generation: the
        // first read after the first dispatch decides.
        assert_eq!(first_request(1, 0, 0..0, &[]), Some(17_000));
    }

    #[test]
    fn sporadic_dispatch_loses_records_of_two_dispatching_intervals_at_most() {
        // Review round 3, the sporadic case: a record at 0.5 s arrives, a
        // record at 1.5 s is lost, and the child is idle from 2 s to
        // 100 s. The rule decides at the first read after the dispatch
        // starts again: the records of 100 s to 107 s are lost too. In
        // time with dispatches, the bound of the timeline test holds.
        let decided = first_request(500, 500, 2_000..100_000, &[]).unwrap();
        assert_eq!(decided, 107_000);
    }

    #[test]
    fn the_gap_is_the_sum_of_the_positive_differences() {
        assert_eq!(gap(dispatched(10, 2, 0), received(0, 7, 3, 0)), 3);
        assert_eq!(gap(dispatched(0, 0, 0), received(0, 5, 5, 5)), 0);
        assert_eq!(
            gap(dispatched(u64::MAX, u64::MAX, 1), received(0, 0, 0, 0)),
            u64::MAX
        );
    }

    #[test]
    fn a_gap_in_one_read_only_is_a_message_in_flight() {
        let mut check = Check::new(at(0));
        assert_eq!(
            check.periodic(dispatched(5, 0, 0), received(1, 3, 0, 0), at(60)),
            None
        );
        assert_eq!(
            check.periodic(dispatched(9, 0, 0), received(1, 9, 0, 0), at(120)),
            None
        );
        assert_eq!(
            check.periodic(dispatched(9, 0, 0), received(1, 9, 0, 0), at(180)),
            None
        );
    }

    #[test]
    fn two_reads_with_a_gap_report_the_smaller_gap_once() {
        let mut check = Check::new(at(0));
        assert_eq!(
            check.periodic(dispatched(10, 0, 0), received(1, 10, 0, 0), at(60)),
            None
        );
        // 3 lost, 2 in flight.
        assert_eq!(
            check.periodic(dispatched(20, 0, 0), received(1, 15, 0, 0), at(120)),
            None
        );
        assert_eq!(
            check.periodic(dispatched(25, 1, 0), received(1, 22, 1, 0), at(180)),
            Some(CountWindow {
                count: 3,
                first: at(60),
                last: at(180),
            })
        );
        // The same gap again is not new.
        assert_eq!(
            check.periodic(dispatched(30, 1, 0), received(1, 27, 1, 0), at(240)),
            None
        );
        // Two more lost, confirmed by two reads.
        assert_eq!(
            check.periodic(dispatched(40, 1, 0), received(1, 35, 1, 0), at(300)),
            None
        );
        assert_eq!(
            check.periodic(dispatched(41, 1, 0), received(1, 36, 1, 0), at(360)),
            Some(CountWindow {
                count: 2,
                first: at(240),
                last: at(360),
            })
        );
    }

    #[test]
    fn a_failed_read_breaks_the_row() {
        let mut check = Check::new(at(0));
        assert_eq!(
            check.periodic(dispatched(5, 0, 0), received(1, 2, 0, 0), at(60)),
            None
        );
        check.read_failed();
        assert_eq!(
            check.periodic(dispatched(5, 0, 0), received(1, 2, 0, 0), at(180)),
            None
        );
        assert!(
            check
                .periodic(dispatched(5, 0, 0), received(1, 2, 0, 0), at(240))
                .is_some()
        );
    }

    #[test]
    fn a_new_child_starts_a_new_generation() {
        let mut check = Check::new(at(0));
        check.periodic(dispatched(5, 0, 0), received(1, 2, 0, 0), at(60));
        assert!(
            check
                .periodic(dispatched(5, 0, 0), received(1, 2, 0, 0), at(120))
                .is_some()
        );
        // The new child starts its counter at 0. A read of the old child
        // with the new counts gives a gap once, which the next read does not
        // confirm.
        assert_eq!(
            check.periodic(dispatched(5, 0, 0), received(2, 0, 0, 0), at(180)),
            None
        );
        assert_eq!(
            check.periodic(dispatched(1, 0, 0), received(2, 1, 0, 0), at(240)),
            None
        );
        // A gap of the new generation is reported from 0 again.
        assert_eq!(
            check.periodic(dispatched(4, 0, 0), received(2, 3, 0, 0), at(300)),
            None
        );
        assert_eq!(
            check.periodic(dispatched(4, 0, 0), received(2, 3, 0, 0), at(360)),
            Some(CountWindow {
                count: 1,
                first: at(240),
                last: at(360),
            })
        );
    }

    #[test]
    fn the_final_read_reports_every_unreported_part_at_once() {
        let mut check = Check::new(at(0));
        assert_eq!(
            check.periodic(dispatched(5, 0, 0), received(1, 3, 0, 0), at(60)),
            None
        );
        assert_eq!(
            check.final_read(dispatched(5, 2, 0), received(1, 3, 1, 0), at(70)),
            Some(CountWindow {
                count: 3,
                first: at(0),
                last: at(70),
            })
        );
        assert_eq!(
            check.final_read(dispatched(5, 2, 0), received(1, 3, 1, 0), at(71)),
            None
        );
    }

    #[test]
    fn a_huge_gap_saturates_the_count() {
        let mut check = Check::new(at(0));
        let window = check
            .final_read(dispatched(u64::MAX, 0, 0), received(1, 0, 0, 0), at(1))
            .unwrap();
        assert_eq!(window.count, u32::MAX);
    }

    /// The real clock and a loopback server that accepts and never answers.
    #[tokio::test]
    async fn the_final_read_does_not_wait_for_a_periodic_read() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });
        let deadline = Duration::from_secs(1);
        let shortfall = Shortfall {
            port,
            period: Duration::from_millis(50),
            max_body: 1024,
            deadline,
            received: Arc::new(Received::default()),
            sink: Arc::new(crate::events::recording::RecordingSink::default()),
            stalled: watch::Sender::new(None),
            grace: Duration::from_millis(10),
            unreadable_after: 2,
            retry: Duration::from_millis(50),
            conditions: test_conditions(),
        };
        let (request, final_read) = oneshot::channel();
        let task = tokio::spawn(shortfall.run(final_read));
        // A periodic read waits for its deadline now.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let requested = std::time::Instant::now();
        request.send(()).unwrap();
        task.await.unwrap();
        // The final read alone: 1 s. After the periodic read: about 1.85 s.
        let elapsed = requested.elapsed();
        assert!(elapsed >= deadline, "{elapsed:?}");
        assert!(elapsed < Duration::from_millis(1_600), "{elapsed:?}");
    }

    fn test_conditions() -> Arc<Conditions> {
        let sink: Arc<dyn EventSink> = Arc::new(crate::events::recording::RecordingSink::default());
        let readiness = Arc::new(crate::readiness::Readiness::new(Arc::clone(&sink)));
        Arc::new(Conditions::new(readiness, sink))
    }

    #[test]
    fn two_failed_reads_in_a_row_make_the_metrics_unreadable_until_a_read_succeeds() {
        let mut readable = Readable::new(2);
        assert!(!readable.failed());
        assert!(!readable.is_unreadable());
        // One slow or failed read is no reason to close the gate.
        assert!(!readable.succeeded());
        assert!(!readable.failed());
        assert!(readable.failed(), "the change to unreadable");
        assert!(readable.is_unreadable());
        assert!(!readable.failed(), "only the change counts");
        assert!(readable.succeeded(), "the change to readable");
        assert!(!readable.is_unreadable());
    }

    #[tokio::test]
    async fn unreadable_metrics_close_condition_2_and_a_read_opens_it() {
        // A port that refuses each connection: the reads fail.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let sink: Arc<dyn EventSink> = Arc::new(crate::events::recording::RecordingSink::default());
        let readiness = Arc::new(crate::readiness::Readiness::new(Arc::clone(&sink)));
        let conditions = Arc::new(Conditions::new(readiness, sink));
        conditions.set_settled(true);
        let shortfall = Shortfall {
            port,
            period: Duration::from_millis(50),
            max_body: 1024,
            deadline: Duration::from_millis(200),
            grace: Duration::from_millis(10),
            unreadable_after: 2,
            retry: Duration::from_millis(20),
            received: Arc::new(Received::default()),
            sink: Arc::new(crate::events::recording::RecordingSink::default()),
            stalled: watch::Sender::new(None),
            conditions: Arc::clone(&conditions),
        };
        let (request, final_read) = oneshot::channel();
        let task = tokio::spawn(shortfall.run(final_read));
        let start = std::time::Instant::now();
        while conditions.pipeline() != crate::readiness::Pipeline::NotReady("metrics_unreadable") {
            assert!(start.elapsed() < Duration::from_secs(5));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // The metrics come back: the next read opens the condition.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0_u8; 1024];
                let _read = stream.read(&mut request).await;
                let body = "dispatch_total{record_type=\"V\"} 0\n";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _written = stream.write_all(response.as_bytes()).await;
            }
        });
        let start = std::time::Instant::now();
        while conditions.pipeline()
            != crate::readiness::Pipeline::NotReady("database_check_pending")
        {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "{:?}",
                conditions.pipeline()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        drop(request);
        task.await.unwrap();
    }

    /// A metrics server whose `V` counter grows by `step` with each read.
    async fn growing_counter(step: u64) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut value = 0_u64;
            while let Ok((mut stream, _)) = listener.accept().await {
                value += step;
                let mut request = [0_u8; 1024];
                let _read = stream.read(&mut request).await;
                let body = format!("dispatch_total{{record_type=\"V\"}} {value}\n");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _written = stream.write_all(response.as_bytes()).await;
            }
        });
        port
    }

    /// Runs the task on the real clock with reads each 50 ms for `run`,
    /// with socket 1 of generation 1 that receives nothing. Returns the
    /// stall request.
    async fn stall_request(step: u64, run: Duration) -> Option<u64> {
        let received = Arc::new(Received::default());
        received.connected(true);
        let (stalled, mut requests) = watch::channel(None);
        let shortfall = Shortfall {
            port: growing_counter(step).await,
            period: Duration::from_millis(50),
            max_body: 1024,
            deadline: Duration::from_secs(1),
            received,
            sink: Arc::new(crate::events::recording::RecordingSink::default()),
            stalled,
            grace: Duration::from_millis(10),
            unreadable_after: 2,
            retry: Duration::from_millis(50),
            conditions: test_conditions(),
        };
        let (request, final_read) = oneshot::channel();
        let task = tokio::spawn(shortfall.run(final_read));
        tokio::time::sleep(run).await;
        drop(request);
        task.await.unwrap();
        *requests.borrow_and_update()
    }

    #[tokio::test]
    async fn a_record_that_arrives_during_a_read_is_no_stall() {
        // The first response says 0 dispatched records, and one record
        // arrives while the response is on its way; later responses say 1.
        // With the received count after the response, the rule would see
        // the record before the child counts it, and then a dispatched
        // record that never arrives.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let received = Arc::new(Received::default());
        received.connected(true);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let counts = Arc::clone(&received);
        tokio::spawn(async move {
            let mut first = true;
            while let Ok((mut stream, _)) = listener.accept().await {
                let value = u64::from(!first);
                if first {
                    counts.count(b"tesla_telemetry_V");
                    first = false;
                }
                let mut request = [0_u8; 1024];
                let _read = stream.read(&mut request).await;
                let body = format!("dispatch_total{{record_type=\"V\"}} {value}\n");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _written = stream.write_all(response.as_bytes()).await;
            }
        });
        let (stalled, requests) = watch::channel(None);
        let shortfall = Shortfall {
            port,
            period: Duration::from_millis(50),
            max_body: 1024,
            deadline: Duration::from_secs(1),
            received,
            sink: Arc::new(crate::events::recording::RecordingSink::default()),
            stalled,
            grace: Duration::from_millis(10),
            unreadable_after: 2,
            retry: Duration::from_millis(50),
            conditions: test_conditions(),
        };
        let (request, final_read) = oneshot::channel();
        let task = tokio::spawn(shortfall.run(final_read));
        tokio::time::sleep(Duration::from_millis(500)).await;
        drop(request);
        task.await.unwrap();
        assert_eq!(*requests.borrow(), None);
    }

    #[tokio::test]
    async fn a_record_in_flight_at_a_read_arrives_within_the_grace() {
        // The second response says 1 dispatched record, and that record
        // reaches the reader 100 ms after the response. The grace of
        // 300 ms sees it; without the grace, the rule would see a
        // dispatched record that never arrived.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let received = Arc::new(Received::default());
        received.connected(true);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let counts = Arc::clone(&received);
        tokio::spawn(async move {
            let mut requests = 0_u32;
            while let Ok((mut stream, _)) = listener.accept().await {
                requests += 1;
                let value = u64::from(requests >= 2);
                if requests == 2 {
                    let counts = Arc::clone(&counts);
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        counts.count(b"tesla_telemetry_V");
                    });
                }
                let mut request = [0_u8; 1024];
                let _read = stream.read(&mut request).await;
                let body = format!("dispatch_total{{record_type=\"V\"}} {value}\n");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _written = stream.write_all(response.as_bytes()).await;
            }
        });
        let (stalled, requests) = watch::channel(None);
        let shortfall = Shortfall {
            port,
            period: Duration::from_millis(500),
            max_body: 1024,
            deadline: Duration::from_secs(1),
            grace: Duration::from_millis(300),
            unreadable_after: 2,
            retry: Duration::from_millis(50),
            received,
            sink: Arc::new(crate::events::recording::RecordingSink::default()),
            stalled,
            conditions: test_conditions(),
        };
        let (request, final_read) = oneshot::channel();
        let task = tokio::spawn(shortfall.run(final_read));
        tokio::time::sleep(Duration::from_millis(2_500)).await;
        drop(request);
        task.await.unwrap();
        assert_eq!(*requests.borrow(), None);
    }

    #[tokio::test]
    async fn the_task_asks_for_a_new_socket_only_while_the_child_dispatches() {
        assert_eq!(stall_request(5, Duration::from_millis(400)).await, Some(1));
        assert_eq!(stall_request(0, Duration::from_millis(400)).await, None);
    }
}

/// The stall rule and the unreadable state through the production read
/// loop ([`Shortfall::run`]) with a scripted metrics server on the
/// loopback interface, scaled times on the real clock, and a child that
/// dispatches each millisecond while the gate is open (review round 4 of
/// PR #22, note N14). The times are scaled by 1/37.5 for `P` and `G` and
/// by 1/7.5 for `D`; the assertions use the formulas of ADR 0001, "SUB
/// socket", "Detection bound", with these times and a margin for the
/// scheduling of the test machine.
#[cfg(test)]
mod loop_tests {
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
    use std::time::Instant;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::time::sleep;

    use super::*;
    use crate::events::recording::RecordingSink;
    use crate::pipeline::conditions::Database;
    use crate::readiness::Readiness;

    /// The read period.
    const P: Duration = Duration::from_millis(400);
    /// The read deadline.
    const D: Duration = Duration::from_millis(100);
    /// The grace.
    const G: Duration = Duration::from_millis(50);
    /// The period of the reads while the metrics are unreadable.
    const R: Duration = Duration::from_millis(40);
    /// A slow answer inside the deadline.
    const SLOW: Duration = Duration::from_millis(70);
    /// The scheduling margin of the assertions.
    const MARGIN: Duration = Duration::from_millis(60);
    /// The read at which the stall starts, just after its snapshot.
    const STALL_AT: usize = 1;

    /// The answer of one scripted read.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Answer {
        Fast,
        Slow,
        Fail,
    }

    use Answer::{Fail as F, Fast as S, Slow as W};

    /// The child, the reader counts, and the gate of one run.
    struct World {
        readiness: Arc<Readiness>,
        conditions: Arc<Conditions>,
        received: Arc<Received>,
        dispatched: AtomicU64,
        stalled: AtomicBool,
        stall_started: std::sync::Mutex<Option<Instant>>,
        accepted: AtomicUsize,
        fails_open: AtomicU32,
    }

    impl World {
        fn new() -> Arc<Self> {
            let sink: Arc<dyn EventSink> = Arc::new(RecordingSink::default());
            let readiness = Arc::new(Readiness::new(Arc::clone(&sink)));
            readiness.set_child_running(true);
            let conditions = Arc::new(Conditions::new(Arc::clone(&readiness), sink));
            conditions.set_settled(true);
            conditions.set_database(Database::Passed);
            let received = Arc::new(Received::default());
            received.connected(true);
            Arc::new(Self {
                readiness,
                conditions,
                received,
                dispatched: AtomicU64::new(0),
                stalled: AtomicBool::new(false),
                stall_started: std::sync::Mutex::new(None),
                accepted: AtomicUsize::new(0),
                fails_open: AtomicU32::new(0),
            })
        }

        fn open(&self) -> bool {
            self.readiness.state().is_open()
        }

        /// A metrics server that answers the reads after `stall_at` with
        /// `script`, and fast before and after it.
        async fn serve(self: &Arc<Self>, script: Vec<Answer>, stall_at: usize) -> u16 {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let port = listener.local_addr().unwrap().port();
            let world = Arc::clone(self);
            tokio::spawn(async move {
                while let Ok((mut stream, _)) = listener.accept().await {
                    let index = world.accepted.fetch_add(1, Ordering::SeqCst);
                    let answer = match index.cmp(&stall_at) {
                        std::cmp::Ordering::Equal => {
                            // The worst phase: the last record arrives just
                            // after the snapshot before this request.
                            world.dispatched.fetch_add(1, Ordering::SeqCst);
                            world.received.count(b"tesla_telemetry_V");
                            world.stalled.store(true, Ordering::SeqCst);
                            *world.stall_started.lock().unwrap() = Some(Instant::now());
                            S
                        }
                        std::cmp::Ordering::Greater => {
                            script.get(index - stall_at - 1).copied().unwrap_or(S)
                        }
                        std::cmp::Ordering::Less => S,
                    };
                    if answer == F && world.open() {
                        world.fails_open.fetch_add(1, Ordering::SeqCst);
                    }
                    let world = Arc::clone(&world);
                    tokio::spawn(async move {
                        let mut request = [0_u8; 1024];
                        let _read = stream.read(&mut request).await;
                        match answer {
                            F => {
                                // No answer: the read ends at its deadline.
                                sleep(D * 3).await;
                                return;
                            }
                            W => sleep(SLOW).await,
                            S => {}
                        }
                        let value = world.dispatched.load(Ordering::SeqCst);
                        let body = format!("dispatch_total{{record_type=\"V\"}} {value}\n");
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        );
                        let _written = stream.write_all(response.as_bytes()).await;
                    });
                }
            });
            port
        }

        fn shortfall(
            self: &Arc<Self>,
            port: u16,
            stalled: watch::Sender<Option<u64>>,
        ) -> Shortfall {
            Shortfall {
                port,
                period: P,
                max_body: 1024,
                deadline: D,
                grace: G,
                unreadable_after: 2,
                retry: R,
                received: Arc::clone(&self.received),
                sink: Arc::new(RecordingSink::default()),
                stalled,
                conditions: Arc::clone(&self.conditions),
            }
        }
    }

    /// What one run shows.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Outcome {
        /// The time with the gate open from the start of the stall to the
        /// decision.
        lost: Duration,
        /// The reads that failed while the gate was open.
        fails_open: u32,
        /// The openings of the gate in that time.
        openings: u32,
    }

    /// Runs the loop until the stall rule decides, at most 20 s.
    async fn run(script: Vec<Answer>) -> Option<Outcome> {
        let world = World::new();
        let port = world.serve(script, STALL_AT).await;
        let (stalled, mut requests) = watch::channel(None);
        let (final_read, final_requests) = oneshot::channel::<()>();
        let task = tokio::spawn(world.shortfall(port, stalled).run(final_requests));
        let changes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut gate = world.readiness.subscribe();
        let recorder = {
            let changes = Arc::clone(&changes);
            tokio::spawn(async move {
                while gate.changed().await.is_ok() {
                    let open = gate.borrow_and_update().is_open();
                    changes.lock().unwrap().push((Instant::now(), open));
                }
            })
        };
        // The child: one record each millisecond while the gate is open.
        let child = {
            let world = Arc::clone(&world);
            tokio::spawn(async move {
                loop {
                    sleep(Duration::from_millis(1)).await;
                    if world.open() {
                        world.dispatched.fetch_add(1, Ordering::SeqCst);
                        if !world.stalled.load(Ordering::SeqCst) {
                            world.received.count(b"tesla_telemetry_V");
                        }
                    }
                }
            })
        };
        // The reader clears condition 2 at once at a stall request.
        let decided = tokio::time::timeout(Duration::from_secs(20), async {
            while requests.changed().await.is_ok() {
                if requests.borrow_and_update().is_some() {
                    world.conditions.set_stalled();
                    return Instant::now();
                }
            }
            Instant::now()
        })
        .await
        .ok();
        child.abort();
        drop(final_read);
        task.await.unwrap();
        recorder.abort();
        let decided = decided?;
        let start = world.stall_started.lock().unwrap().unwrap();
        let mut lost = Duration::ZERO;
        let mut openings = 1;
        let mut open_since = Some(start);
        for &(at, open) in changes.lock().unwrap().iter() {
            if at < start || at > decided {
                continue;
            }
            match (open, open_since) {
                (true, None) => {
                    open_since = Some(at);
                    openings += 1;
                }
                (false, Some(since)) => {
                    lost += at - since;
                    open_since = None;
                }
                _ => {}
            }
        }
        if let Some(since) = open_since {
            lost += decided - since;
        }
        Some(Outcome {
            lost,
            fails_open: world.fails_open.load(Ordering::SeqCst),
            openings,
        })
    }

    /// `n` periods plus `d` deadlines plus one grace.
    fn bound(n: u32, d: u32) -> Duration {
        P * n + D * d + G
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_loop_meets_the_bound_with_zero_one_and_two_failed_reads() {
        // The gate stays open: (2 + f) P + D + G, f failed reads.
        let cases = [
            (vec![], 0, bound(2, 1)),
            (vec![S, W], 0, bound(2, 1)),
            (vec![F], 1, bound(3, 1)),
            (vec![F, S, W], 1, bound(3, 1)),
            (vec![F, S, F], 2, bound(4, 1)),
            (vec![F, S, F, W], 2, bound(4, 1)),
        ];
        for (script, failed, limit) in cases {
            let outcome = run(script.clone())
                .await
                .unwrap_or_else(|| panic!("{script:?}"));
            eprintln!("LOOP {script:?} {outcome:?}");
            assert_eq!(outcome.fails_open, failed, "{script:?}: {outcome:?}");
            assert_eq!(outcome.openings, 1, "{script:?}: {outcome:?}");
            assert!(outcome.lost < limit + MARGIN, "{script:?}: {outcome:?}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_stall_over_episodes_of_unreadable_metrics_meets_the_total_bound() {
        // Review round 4, finding B4: the read that ends an episode of
        // unreadable metrics cannot decide, so more failed reads can follow
        // with the gate open. Up to four failed reads with the gate open,
        // up to three openings, and less than 4P + 2D + G in total.
        let cases = [
            (vec![F, S, F, F, S], 3, 2),
            (vec![F, F, S, F, S], 3, 2),
            (vec![F, F, S, F, W], 3, 2),
            (vec![F, F, S, F, F, S], 4, 3),
        ];
        for (script, failed, openings) in cases {
            let outcome = run(script.clone())
                .await
                .unwrap_or_else(|| panic!("{script:?}"));
            eprintln!("LOOP {script:?} {outcome:?}");
            assert_eq!(outcome.fails_open, failed, "{script:?}: {outcome:?}");
            assert_eq!(outcome.openings, openings, "{script:?}: {outcome:?}");
            assert!(
                outcome.lost < bound(4, 2) + MARGIN,
                "{script:?}: {outcome:?}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "an evidence run of about 10 minutes: every pattern of seven reads"]
    async fn every_pattern_of_seven_reads_meets_the_total_bound() {
        let mut worst = (Duration::ZERO, Vec::new());
        for ok in [S, W] {
            for bits in 0_u32..128 {
                let script: Vec<Answer> = (0..7)
                    .map(|read| if bits & (1 << read) == 0 { ok } else { F })
                    .collect();
                let outcome = run(script.clone())
                    .await
                    .unwrap_or_else(|| panic!("{script:?}"));
                eprintln!("PATTERN {script:?} {outcome:?}");
                assert!(
                    outcome.lost < bound(4, 2) + MARGIN,
                    "{script:?}: {outcome:?}"
                );
                assert!(outcome.fails_open <= 4, "{script:?}: {outcome:?}");
                assert!(outcome.openings <= 3, "{script:?}: {outcome:?}");
                if outcome.lost > worst.0 {
                    worst = (outcome.lost, script);
                }
            }
        }
        eprintln!(
            "WORST {worst:?}; 4P + D + G = {:?}; 4P + 2D + G = {:?}",
            bound(4, 1),
            bound(4, 2)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn after_unreadable_metrics_the_reads_come_each_retry_period() {
        // No stall. Two failed reads close the gate; the next read comes
        // one retry period (here 40 ms) later, not one read period
        // (400 ms), and its answer opens the gate.
        let world = World::new();
        let port = world.serve(vec![F, F], 0).await;
        let (stalled, _requests) = watch::channel(None);
        let (final_read, final_requests) = oneshot::channel::<()>();
        let task = tokio::spawn(world.shortfall(port, stalled).run(final_requests));
        let mut gate = world.readiness.subscribe();
        tokio::time::timeout(Duration::from_secs(5), gate.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(!gate.borrow_and_update().is_open());
        let closed = Instant::now();
        tokio::time::timeout(Duration::from_secs(5), gate.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(gate.borrow_and_update().is_open());
        let reopened = closed.elapsed();
        assert!(reopened >= R, "{reopened:?}");
        assert!(reopened < R + MARGIN * 2, "{reopened:?}");
        drop(final_read);
        task.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_shutdown_during_the_grace_ends_the_loop_at_once() {
        let world = World::new();
        let port = world.serve(Vec::new(), usize::MAX).await;
        let (stalled, _requests) = watch::channel(None);
        let shortfall = Shortfall {
            period: Duration::from_secs(1),
            grace: Duration::from_millis(900),
            ..world.shortfall(port, stalled)
        };
        let (final_read, final_requests) = oneshot::channel();
        let task = tokio::spawn(shortfall.run(final_requests));
        // The first read at 1 s; its grace lasts until about 1.9 s.
        sleep(Duration::from_millis(1_150)).await;
        assert_eq!(world.accepted.load(Ordering::SeqCst), 1);
        let requested = Instant::now();
        final_read.send(()).unwrap();
        task.await.unwrap();
        // The final read follows at once; without the stop in the grace,
        // the task would wait for the rest of the grace first.
        let elapsed = requested.elapsed();
        assert!(elapsed < Duration::from_millis(400), "{elapsed:?}");
        assert_eq!(world.accepted.load(Ordering::SeqCst), 2);
    }
}
