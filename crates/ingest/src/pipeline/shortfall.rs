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

/// The longest wait between two stall requests, in reads.
const MAX_STALL_WAIT_READS: u32 = 64;

/// The stall rule of the SUB socket (ADR 0001, "SUB socket"): a liveness
/// rule that needs no event of the `zeromq` crate.
///
/// - **Rule.** Three reads `a`, `b`, and `k` of one socket, one period
///   apart. When the child dispatched records between `a` and `b`, and
///   the reader received no record between `a` and `k`, the socket is
///   stalled. A live subscription receives every record that the child
///   dispatched at least one period before: a drop at the libzmq high
///   water mark drops the newest records, and the queued older ones still
///   arrive. So the rule needs both conditions, and an idle child (no new
///   dispatch) never gives a stall.
/// - **Start.** For a new generation, the first point is zero: the counter
///   of a new child and the received counts both start at 0, and no record
///   passes the closed gate before the socket settles. For a new socket of
///   the same generation, the first point is the last read of the old
///   socket. A counter that goes down (a read of the old child) and a
///   failed read start the points again.
/// - **Backoff.** After the `n`-th stall request in a row without a
///   received record, the next request waits `2^n` reads, at most
///   [`MAX_STALL_WAIT_READS`]. A received record ends the row.
#[derive(Debug, Clone, Default)]
pub struct Liveness {
    key: Option<(u64, u64)>,
    points: Vec<Point>,
    stalls: u32,
    wait_reads: u32,
}

impl Liveness {
    /// A periodic read. Returns the number of the stalled socket.
    pub fn observe(&mut self, dispatched: Dispatched, received: Counts) -> Option<u64> {
        let point = Point::of(dispatched, received);
        let key = (received.generation, received.socket);
        if self.key != Some(key) {
            let same_generation = self
                .key
                .is_some_and(|(generation, _)| generation == received.generation);
            let seed = if same_generation {
                self.points.last().copied()
            } else {
                self.stalls = 0;
                self.wait_reads = 0;
                Some(Point::ZERO)
            };
            self.points = seed.into_iter().collect();
            self.key = Some(key);
        }
        if let Some(last) = self.points.last().copied() {
            if point.dispatched < last.dispatched {
                // A read of another child: start again.
                self.points.clear();
            } else if point.received > last.received {
                self.stalls = 0;
                self.wait_reads = 0;
            }
        }
        let stalled = match self.points.as_slice() {
            [.., a, b] => b.dispatched > a.dispatched && point.received == a.received,
            _ => false,
        };
        self.points.push(point);
        if self.points.len() > 2 {
            self.points.remove(0);
        }
        self.wait_reads = self.wait_reads.saturating_sub(1);
        if !stalled || self.wait_reads > 0 {
            return None;
        }
        self.stalls = self.stalls.saturating_add(1);
        self.wait_reads = 2_u32.saturating_pow(self.stalls).min(MAX_STALL_WAIT_READS);
        Some(key.1)
    }

    /// A read failed: start the points again.
    pub fn read_failed(&mut self) {
        self.points.clear();
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
    /// The counts of the reader.
    pub received: Arc<Received>,
    /// The sink of the `zmq_shortfall` events.
    pub sink: Arc<dyn EventSink>,
    /// The number of the socket that the stall rule found stalled. The
    /// reader replaces that socket ([`super::reader`]).
    pub stalled: watch::Sender<Option<u64>>,
}

impl Shortfall {
    /// Runs the task. `final_read` asks for the final read.
    pub async fn run(self, mut final_read: oneshot::Receiver<()>) {
        let mut check = Check::new(Timestamp::now());
        let mut liveness = Liveness::default();
        let mut tick =
            tokio::time::interval_at(tokio::time::Instant::now() + self.period, self.period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // A dropped sender also asks for the end, without a read.
        let requested = loop {
            tokio::select! {
                _ = tick.tick() => {
                    // The request of the final read stops a periodic read,
                    // so that the final read gets its whole step bound.
                    let read = metrics::read(self.port, self.max_body, self.deadline);
                    tokio::pin!(read);
                    tokio::select! {
                        read = &mut read => self.observe(&mut check, &mut liveness, read, false),
                        requested = &mut final_read => break requested.is_ok(),
                    }
                }
                requested = &mut final_read => break requested.is_ok(),
            }
        };
        if requested {
            let read = metrics::read(self.port, self.max_body, self.deadline).await;
            self.observe(&mut check, &mut liveness, read, true);
        }
    }

    fn observe(
        &self,
        check: &mut Check,
        liveness: &mut Liveness,
        read: Result<Dispatched, metrics::ReadError>,
        last: bool,
    ) {
        match read {
            Ok(dispatched) => {
                // After the response: see the module documentation.
                let received = self.received.snapshot();
                let now = Timestamp::now();
                let window = if last {
                    check.final_read(dispatched, received, now)
                } else {
                    if let Some(socket) = liveness.observe(dispatched, received) {
                        warn!("subscriber_stalled");
                        self.stalled.send_replace(Some(socket));
                    }
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
                liveness.read_failed();
                warn!(reason = error.as_str(), "shortfall_read_failed");
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

    #[test]
    fn a_socket_that_receives_nothing_while_the_child_dispatches_is_stalled() {
        let mut rule = Liveness::default();
        // A new generation starts at zero. The first read already shows
        // dispatched records; the second read shows that none arrived.
        assert_eq!(rule.observe(dispatched(3, 0, 0), on(1, 1, 0)), None);
        assert_eq!(rule.observe(dispatched(5, 0, 0), on(1, 1, 0)), Some(1));
    }

    #[test]
    fn an_idle_child_or_a_live_socket_is_never_stalled() {
        let mut rule = Liveness::default();
        // Idle: the counter does not grow.
        for _ in 0..10 {
            assert_eq!(rule.observe(dispatched(0, 0, 0), on(1, 1, 0)), None);
        }
        // Live: each dispatched record arrives, also with a delay of one
        // read.
        let mut rule = Liveness::default();
        for read in 1..10 {
            assert_eq!(
                rule.observe(dispatched(read * 10, 0, 0), on(1, 1, (read - 1) * 10)),
                None
            );
        }
        // A drop at the high water mark: the newest records are lost, and
        // the queued older ones still arrive.
        let mut rule = Liveness::default();
        assert_eq!(rule.observe(dispatched(1_000, 0, 0), on(1, 1, 400)), None);
        assert_eq!(rule.observe(dispatched(1_500, 0, 0), on(1, 1, 1_000)), None);
        assert_eq!(rule.observe(dispatched(1_500, 0, 0), on(1, 1, 1_000)), None);
        assert_eq!(rule.observe(dispatched(1_500, 0, 0), on(1, 1, 1_000)), None);
    }

    #[test]
    fn a_new_socket_starts_from_the_last_read_and_stalls_back_off() {
        let mut rule = Liveness::default();
        assert_eq!(rule.observe(dispatched(3, 0, 0), on(1, 1, 0)), None);
        assert_eq!(rule.observe(dispatched(6, 0, 0), on(1, 1, 0)), Some(1));
        // Socket 2 of the same child: the last read of socket 1 is its
        // first point, so the rule needs two more reads.
        let mut requests = Vec::new();
        for read in 0..40_u64 {
            let at = 9 + read * 3;
            requests.push(rule.observe(dispatched(at, 0, 0), on(1, 2 + read / 100, 0)));
        }
        // The first request for socket 2 after two reads, then the waits
        // double: 4 reads, 8 reads, 16 reads.
        let at: Vec<usize> = requests
            .iter()
            .enumerate()
            .filter_map(|(read, request)| request.map(|_| read))
            .collect();
        assert_eq!(at, [1, 5, 13, 29]);
        // A received record ends the row of stalls: the next stall is
        // requested without a wait.
        assert_eq!(rule.observe(dispatched(200, 0, 0), on(1, 3, 1)), None);
        assert_eq!(rule.observe(dispatched(203, 0, 0), on(1, 3, 1)), None);
        assert_eq!(rule.observe(dispatched(206, 0, 0), on(1, 3, 1)), Some(3));
    }

    #[test]
    fn a_new_child_a_read_of_the_old_child_or_a_failed_read_start_again() {
        let mut rule = Liveness::default();
        assert_eq!(rule.observe(dispatched(50, 0, 0), on(1, 1, 50)), None);
        // A late read of the old child with the counts of the new one.
        assert_eq!(rule.observe(dispatched(60, 0, 0), on(2, 2, 0)), None);
        // The new child: its counter is below the old one.
        assert_eq!(rule.observe(dispatched(0, 0, 0), on(2, 2, 0)), None);
        assert_eq!(rule.observe(dispatched(0, 0, 0), on(2, 2, 0)), None);
        // A failed read: the next two reads cannot decide.
        assert_eq!(rule.observe(dispatched(5, 0, 0), on(2, 2, 0)), None);
        rule.read_failed();
        assert_eq!(rule.observe(dispatched(10, 0, 0), on(2, 2, 0)), None);
        assert_eq!(rule.observe(dispatched(15, 0, 0), on(2, 2, 0)), None);
        assert_eq!(rule.observe(dispatched(20, 0, 0), on(2, 2, 0)), Some(2));
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
        };
        let (request, final_read) = oneshot::channel();
        let task = tokio::spawn(shortfall.run(final_read));
        tokio::time::sleep(run).await;
        drop(request);
        task.await.unwrap();
        *requests.borrow_and_update()
    }

    #[tokio::test]
    async fn the_task_asks_for_a_new_socket_only_while_the_child_dispatches() {
        assert_eq!(stall_request(5, Duration::from_millis(400)).await, Some(1));
        assert_eq!(stall_request(0, Duration::from_millis(400)).await, None);
    }
}
