//! Tests of the writer with a store in memory and a paused clock.
//!
//! The store follows a script: each call takes the next step of its list,
//! and a call without a step succeeds. The store records each call with the
//! time of the paused clock, so the tests check the cadence to the
//! millisecond.

use std::collections::VecDeque;
use std::sync::Mutex;

use jiff::Timestamp;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use super::*;
use crate::events::EventSink;
use crate::pipeline::queue::Item;
use crate::readiness::Readiness;
use crate::record::{
    Decoded, Signal, SignalValue, Unprocessed, UnprocessedReason, VehicleData, Vin,
};
use crate::storage::SqlState;
use crate::watchdog::{Task, WRITER, Watchdog};

#[derive(Debug, Clone, Copy)]
enum Step {
    Ok,
    /// Succeeds after `count` transactions of `each`, with a progress call
    /// before each one.
    Slow {
        count: u32,
        each: Duration,
    },
    /// Fails with `error` after `after`: a check without an answer that
    /// waits for the connect timeout.
    Late {
        after: Duration,
        error: StorageError,
    },
    Fail {
        error: StorageError,
        committed: usize,
    },
    Hang,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    Check,
    Write(usize),
    Events(usize),
}

#[derive(Default)]
struct Script {
    checks: VecDeque<Step>,
    writes: VecDeque<Step>,
    events: VecDeque<Step>,
    calls: Vec<(Duration, Call)>,
    records: Vec<Vec<u8>>,
    stored_events: Vec<&'static str>,
}

#[derive(Clone)]
struct FakeStore {
    script: Arc<Mutex<Script>>,
    start: Instant,
}

impl FakeStore {
    fn step(&self, call: Call, list: fn(&mut Script) -> &mut VecDeque<Step>) -> Step {
        let mut script = self.script.lock().unwrap();
        script.calls.push((self.start.elapsed(), call));
        list(&mut script).pop_front().unwrap_or(Step::Ok)
    }
}

impl Store for FakeStore {
    async fn check(&mut self) -> Result<(), StorageError> {
        match self.step(Call::Check, |script| &mut script.checks) {
            Step::Ok | Step::Slow { .. } => Ok(()),
            Step::Late { after, error } => {
                sleep(after).await;
                Err(error)
            }
            Step::Fail { error, .. } => Err(error),
            Step::Hang => std::future::pending().await,
        }
    }

    async fn write(
        &mut self,
        records: &[QueuedRecord<'_>],
        progress: &mut (dyn FnMut() + Send),
    ) -> Result<WriteOutcome, WriteError> {
        let step = self.step(Call::Write(records.len()), |script| &mut script.writes);
        let stored = match step {
            Step::Ok => records.len(),
            Step::Slow { count, each } => {
                for _ in 0..count {
                    progress();
                    sleep(each).await;
                }
                records.len()
            }
            Step::Fail { committed, .. } => committed,
            Step::Late { .. } => panic!("a late step is for checks only"),
            Step::Hang => std::future::pending().await,
        };
        self.script.lock().unwrap().records.extend(
            records[..stored]
                .iter()
                .map(|record| record.payload.to_vec()),
        );
        match step {
            Step::Fail { error, committed } => Err(WriteError {
                error,
                committed,
                outcome: WriteOutcome::default(),
            }),
            _ => Ok(WriteOutcome {
                transactions: 1,
                ..WriteOutcome::default()
            }),
        }
    }

    async fn write_events(
        &mut self,
        events: &[IngestEvent],
        _progress: &mut (dyn FnMut() + Send),
    ) -> Result<EventOutcome, StorageError> {
        match self.step(Call::Events(events.len()), |script| &mut script.events) {
            Step::Ok | Step::Slow { .. } => {
                self.script
                    .lock()
                    .unwrap()
                    .stored_events
                    .extend(events.iter().map(|event| event.kind.name()));
                Ok(EventOutcome {
                    inserted: u64::try_from(events.len()).unwrap(),
                    ..EventOutcome::default()
                })
            }
            Step::Late { .. } => panic!("a late step is for checks only"),
            Step::Fail { error, .. } => Err(error),
            Step::Hang => std::future::pending().await,
        }
    }
}

const NO_ANSWER: StorageError = StorageError {
    class: FailureClass::NoAnswer,
    code: ErrorCode::Io,
};

fn answered(code: &str) -> StorageError {
    StorageError {
        class: FailureClass::Answered,
        code: ErrorCode::Sqlstate(SqlState::parse(code).unwrap()),
    }
}

fn data(code: &str) -> StorageError {
    StorageError {
        class: FailureClass::Data,
        code: ErrorCode::Sqlstate(SqlState::parse(code).unwrap()),
    }
}

fn fail(error: StorageError) -> Step {
    Step::Fail {
        error,
        committed: 0,
    }
}

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

const INTERVAL: Duration = Duration::from_mins(15);

struct Harness {
    script: Arc<Mutex<Script>>,
    queue: Arc<Queue>,
    events: Arc<EventBuffer>,
    conditions: Arc<Conditions>,
    readiness: Arc<Readiness>,
    shutdown: Option<oneshot::Sender<ShutdownRequest>>,
    task: JoinHandle<()>,
}

fn start(script: Script, limits: Limits) -> Harness {
    let script = Arc::new(Mutex::new(script));
    let wake = Arc::new(Notify::new());
    let events = Arc::new(EventBuffer::new(limits.max_events, Arc::clone(&wake)));
    let sink: Arc<dyn EventSink> = Arc::clone(&events) as Arc<dyn EventSink>;
    let readiness = Arc::new(Readiness::new(Arc::clone(&sink)));
    readiness.set_child_running(true);
    let conditions = Arc::new(Conditions::new(Arc::clone(&readiness), Arc::clone(&sink)));
    conditions.set_settled(true);
    let queue = Arc::new(Queue::new(
        &limits,
        Arc::clone(&conditions),
        Arc::clone(&wake),
    ));
    let writer = Writer {
        store: FakeStore {
            script: Arc::clone(&script),
            start: Instant::now(),
        },
        queue: Arc::clone(&queue),
        events: Arc::clone(&events),
        conditions: Arc::clone(&conditions),
        wake,
        limits,
        flush_interval: INTERVAL,
        jitter: || 0.0,
    };
    let (shutdown, requests) = oneshot::channel();
    let heartbeat = Watchdog::new().register(WRITER);
    let task = tokio::spawn(writer.run(heartbeat, requests));
    Harness {
        script,
        queue,
        events,
        conditions,
        readiness,
        shutdown: Some(shutdown),
        task,
    }
}

impl Harness {
    fn calls(&self) -> Vec<(Duration, Call)> {
        self.script.lock().unwrap().calls.clone()
    }

    fn checks(&self) -> Vec<Duration> {
        self.calls()
            .into_iter()
            .filter(|(_, call)| *call == Call::Check)
            .map(|(at, _)| at)
            .collect()
    }

    fn writes(&self) -> Vec<(Duration, usize)> {
        self.calls()
            .into_iter()
            .filter_map(|(at, call)| match call {
                Call::Write(count) => Some((at, count)),
                _ => None,
            })
            .collect()
    }

    fn records(&self) -> Vec<Vec<u8>> {
        self.script.lock().unwrap().records.clone()
    }

    fn stored_events(&self) -> Vec<&'static str> {
        self.script.lock().unwrap().stored_events.clone()
    }

    fn push(&self, payload: &str, rows: usize) {
        assert!(self.queue.push(item(payload, rows)));
    }

    async fn shut_down(&mut self, deadline: Duration) -> Report {
        let (done, report) = oneshot::channel();
        self.shutdown
            .take()
            .unwrap()
            .send(ShutdownRequest { deadline, done })
            .unwrap();
        let report = report.await.unwrap();
        (&mut self.task).await.unwrap();
        report
    }
}

/// An item with `rows` signal rows, or an unprocessed message without
/// rows. The payload names the item.
fn item(payload: &str, rows: usize) -> Item {
    let decoded = if rows == 0 {
        Decoded::Unprocessed(Unprocessed {
            reason: UnprocessedReason::DecodeFailed,
            received_at: Timestamp::UNIX_EPOCH,
        })
    } else {
        Decoded::VehicleData(VehicleData {
            vin: Vin::new("TESTVIN0000000001".to_owned()),
            created_at: Timestamp::UNIX_EPOCH,
            received_at: Timestamp::UNIX_EPOCH,
            is_resend: false,
            signals: (0..rows)
                .map(|field| Signal {
                    field: i32::try_from(field).unwrap(),
                    value: SignalValue::Bool(true),
                })
                .collect(),
            unknown_value_variants: 0,
            unknown_fields: false,
        })
    };
    Item::new(
        b"tesla_telemetry_V".to_vec(),
        payload.as_bytes().to_vec(),
        decoded,
    )
}

fn payloads(names: &[&str]) -> Vec<Vec<u8>> {
    names.iter().map(|name| name.as_bytes().to_vec()).collect()
}

#[tokio::test(start_paused = true)]
async fn the_check_passes_and_the_events_are_stored_at_once() {
    let mut harness = start(Script::default(), Limits::PRODUCTION);
    harness.events.record(IngestEvent::now(EventKind::Started));
    sleep(Duration::from_millis(10)).await;
    assert_eq!(harness.conditions.database(), Database::Passed);
    assert!(harness.readiness.state().is_healthy());
    assert_eq!(harness.checks(), [Duration::ZERO]);
    assert_eq!(harness.stored_events(), ["started", "gate_opened"]);
    assert!(harness.events.is_empty());
    let report = harness.shut_down(secs(20)).await;
    assert!(report.is_complete(), "{report:?}");
}

#[tokio::test(start_paused = true)]
async fn no_answer_for_120_s_enters_degraded_mode_and_a_later_check_ends_it() {
    let script = Script {
        checks: std::iter::repeat_n(fail(NO_ANSWER), 8).collect(),
        ..Script::default()
    };
    let mut harness = start(script, Limits::PRODUCTION);
    harness.push("a", 1);

    sleep(secs(119)).await;
    assert_eq!(harness.conditions.database(), Database::Pending);
    assert!(!harness.readiness.state().is_open());
    sleep(Duration::from_millis(1_001)).await;
    // Degraded mode: the gate opens, `/health` fails, and the records wait.
    assert_eq!(harness.conditions.database(), Database::Degraded);
    assert!(harness.readiness.state().is_open());
    assert!(!harness.readiness.state().is_healthy());
    assert_eq!(harness.writes(), Vec::<(Duration, usize)>::new());

    // The checks: 1 s, doubling. The ninth check passes at 255 s.
    sleep(secs(200)).await;
    let checks: Vec<u64> = harness.checks().iter().map(Duration::as_secs).collect();
    assert_eq!(checks, [0, 1, 3, 7, 15, 31, 63, 127, 255]);
    assert_eq!(harness.conditions.database(), Database::Passed);
    assert!(harness.readiness.state().is_healthy());
    // The check runs before the first write, and the flush follows at once.
    assert_eq!(harness.writes(), [(secs(255), 1)]);
    assert_eq!(harness.records(), payloads(&["a"]));
    // The `degraded_mode` event waited in memory and is stored now.
    assert!(harness.stored_events().contains(&"degraded_mode"));
    harness.shut_down(secs(20)).await;
}

#[tokio::test(start_paused = true)]
async fn degraded_mode_starts_during_a_check_that_waits_for_its_connect_timeout() {
    // Each check without an answer waits 15 s, the connect timeout.
    let late = Step::Late {
        after: secs(15),
        error: NO_ANSWER,
    };
    let script = Script {
        checks: std::iter::repeat_n(late, 20).collect(),
        ..Script::default()
    };
    let limits = Limits {
        degraded_after: secs(5),
        ..Limits::PRODUCTION
    };
    let mut harness = start(script, limits);
    sleep(Duration::from_millis(4_999)).await;
    assert_eq!(harness.conditions.database(), Database::Pending);
    // The first check still waits: the limit counts from its start.
    sleep(Duration::from_millis(2)).await;
    assert_eq!(harness.conditions.database(), Database::Degraded);
    assert_eq!(harness.checks(), [Duration::ZERO]);
    harness.shut_down(secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn an_answered_failure_keeps_the_gate_closed_and_slows_to_60_minutes() {
    let script = Script {
        checks: std::iter::repeat_n(fail(answered("28P01")), 4).collect(),
        ..Script::default()
    };
    let mut harness = start(script, Limits::PRODUCTION);
    sleep(secs(200)).await;
    // Not degraded: the database answered.
    assert_eq!(harness.conditions.database(), Database::Refused);
    assert_eq!(
        harness.readiness.state().closed_reason(),
        Some("database_refused")
    );
    sleep(Duration::from_mins(130)).await;
    let checks: Vec<u64> = harness.checks().iter().map(Duration::as_secs).collect();
    // 0, then 1 s, 2 s, then 60 minutes after the third equal failure.
    assert_eq!(checks, [0, 1, 3, 3_603, 7_203]);
    assert_eq!(harness.conditions.database(), Database::Passed);
    // The refused state has a next check, so it does not last.
    assert!(harness.readiness.state().is_healthy());
    harness.shut_down(secs(20)).await;
}

#[tokio::test(start_paused = true)]
async fn an_answered_failure_in_degraded_mode_closes_the_gate_at_once() {
    let script = Script {
        checks: std::iter::repeat_n(fail(NO_ANSWER), 7)
            .chain([fail(answered("42501"))])
            .collect(),
        ..Script::default()
    };
    let mut harness = start(script, Limits::PRODUCTION);
    sleep(secs(121)).await;
    assert!(harness.readiness.state().is_open());
    // The eighth check at 127 s is answered.
    sleep(secs(7)).await;
    assert_eq!(
        harness.readiness.state().closed_reason(),
        Some("database_refused")
    );
    harness.shut_down(secs(20)).await;
}

#[tokio::test(start_paused = true)]
async fn the_flush_waits_for_the_interval_of_the_oldest_item() {
    let mut harness = start(Script::default(), Limits::PRODUCTION);
    sleep(secs(1)).await;
    harness.push("a", 1);
    sleep(secs(100)).await;
    harness.push("b", 1);
    sleep(secs(799)).await;
    assert_eq!(harness.writes(), Vec::<(Duration, usize)>::new());
    sleep(secs(2)).await;
    // The flush at 901 s writes both items.
    assert_eq!(harness.writes(), [(secs(901), 2)]);
    harness.shut_down(secs(20)).await;
}

#[tokio::test(start_paused = true)]
async fn an_event_alone_also_ages_into_a_flush() {
    let mut harness = start(Script::default(), Limits::PRODUCTION);
    sleep(secs(10)).await;
    harness
        .events
        .record(IngestEvent::now(EventKind::GateClosed { reason: "x" }));
    sleep(secs(899)).await;
    assert_eq!(harness.stored_events(), ["gate_opened"]);
    sleep(secs(2)).await;
    assert_eq!(harness.stored_events(), ["gate_opened", "gate_closed"]);
    harness.shut_down(secs(20)).await;
}

#[tokio::test(start_paused = true)]
async fn five_thousand_signal_rows_and_the_high_mark_flush_at_once() {
    let limits = Limits {
        flush_rows: 10,
        ..Limits::PRODUCTION
    };
    let mut harness = start(Script::default(), limits);
    sleep(secs(5)).await;
    harness.push("a", 6);
    sleep(secs(5)).await;
    assert_eq!(harness.writes(), Vec::<(Duration, usize)>::new());
    harness.push("b", 6);
    sleep(Duration::from_millis(1)).await;
    assert_eq!(harness.writes(), [(secs(10), 2)]);

    // The high mark: items without signal rows.
    let small = Limits {
        queue_bytes: 4 * item("x", 0).bytes(),
        ..Limits::PRODUCTION
    };
    let mut harness2 = start(Script::default(), small);
    sleep(secs(5)).await;
    for name in ["a", "b", "c"] {
        harness2.push(name, 0);
    }
    sleep(Duration::from_millis(1)).await;
    assert_eq!(harness2.writes(), [(secs(5), 3)]);
    assert!(harness2.readiness.state().is_open());
    harness.shut_down(secs(20)).await;
    harness2.shut_down(secs(20)).await;
}

#[tokio::test(start_paused = true)]
async fn a_failed_write_keeps_the_records_and_retries_on_the_cadence() {
    let small = Limits {
        queue_bytes: 4 * item("x", 0).bytes(),
        ..Limits::PRODUCTION
    };
    let script = Script {
        writes: std::iter::repeat_n(fail(NO_ANSWER), 3).collect(),
        ..Script::default()
    };
    let mut harness = start(script, small);
    sleep(secs(5)).await;
    // The high mark closes the gate and starts the flush.
    for name in ["a", "b", "c"] {
        harness.push(name, 0);
    }
    sleep(secs(1)).await;
    assert_eq!(
        harness.readiness.state().closed_reason(),
        Some("queue_high_mark")
    );
    sleep(secs(10)).await;
    let writes: Vec<u64> = harness
        .writes()
        .iter()
        .map(|(at, _)| at.as_secs())
        .collect();
    // 5 s, then 1 s, 2 s, and 4 s later.
    assert_eq!(writes, [5, 6, 8, 12]);
    assert_eq!(harness.records(), payloads(&["a", "b", "c"]));
    assert!(harness.queue.is_empty());
    // The write succeeded and the queue is below the low mark.
    assert!(harness.readiness.state().is_open());
    harness.shut_down(secs(20)).await;
}

#[tokio::test(start_paused = true)]
async fn records_that_committed_before_a_failure_leave_the_queue_and_reset_the_cadence() {
    let limits = Limits {
        flush_rows: 1,
        ..Limits::PRODUCTION
    };
    let script = Script {
        writes: [
            fail(answered("53100")),
            fail(answered("53100")),
            Step::Fail {
                error: answered("53100"),
                committed: 1,
            },
            fail(answered("53100")),
        ]
        .into(),
        ..Script::default()
    };
    let mut harness = start(script, limits);
    sleep(secs(1)).await;
    harness.push("a", 1);
    harness.push("b", 1);
    sleep(secs(20)).await;
    let writes: Vec<(u64, usize)> = harness
        .writes()
        .iter()
        .map(|(at, count)| (at.as_secs(), *count))
        .collect();
    // Without the progress of the third write, the fourth attempt would
    // wait 60 minutes. With it, the cadence starts again at 1 s.
    assert_eq!(writes, [(1, 2), (2, 2), (4, 2), (5, 1), (7, 1)]);
    assert_eq!(harness.records(), payloads(&["a", "b"]));
    harness.shut_down(secs(20)).await;
}

#[tokio::test(start_paused = true)]
async fn a_refused_record_is_dropped_with_an_event_and_the_rest_is_written() {
    let limits = Limits {
        flush_rows: 1,
        ..Limits::PRODUCTION
    };
    let script = Script {
        writes: [Step::Fail {
            error: data("23514"),
            committed: 1,
        }]
        .into(),
        ..Script::default()
    };
    let mut harness = start(script, limits);
    sleep(secs(1)).await;
    for name in ["a", "b", "c"] {
        harness.push(name, 1);
    }
    sleep(Duration::from_millis(10)).await;
    // No retry delay: the second write follows at once with the rest.
    assert_eq!(harness.writes(), [(secs(1), 3), (secs(1), 1)]);
    assert_eq!(harness.records(), payloads(&["a", "c"]));
    assert!(harness.queue.is_empty());
    let report = harness.shut_down(secs(20)).await;
    assert!(report.is_complete());
    let dropped = harness
        .stored_events()
        .iter()
        .filter(|kind| **kind == "records_dropped")
        .count();
    assert_eq!(dropped, 1);
}

#[tokio::test(start_paused = true)]
async fn a_statement_timeout_halves_the_next_batch() {
    let limits = Limits {
        flush_rows: 4_000,
        ..Limits::PRODUCTION
    };
    let script = Script {
        writes: [fail(answered("57014")), fail(answered("57014"))].into(),
        ..Script::default()
    };
    let mut harness = start(script, limits);
    sleep(secs(1)).await;
    for index in 0..4 {
        harness.push(&index.to_string(), 1_000);
    }
    sleep(secs(10)).await;
    let counts: Vec<usize> = harness.writes().iter().map(|(_, count)| *count).collect();
    // 4 records of 1,000 rows: a batch of 5,000 rows holds all 4. After a
    // timeout the batch has at most 2,500 rows, then 1,250; the success
    // restores the full size.
    assert_eq!(counts, [4, 2, 1, 3]);
    assert_eq!(harness.records().len(), 4);
    harness.shut_down(secs(20)).await;
}

#[tokio::test(start_paused = true)]
async fn the_final_flush_stores_the_queue_and_the_stopped_event() {
    let mut harness = start(Script::default(), Limits::PRODUCTION);
    sleep(secs(5)).await;
    harness.push("a", 1);
    harness
        .events
        .record(IngestEvent::now(EventKind::Stopped { reason: "SIGTERM" }));
    let report = harness.shut_down(secs(20)).await;
    assert!(report.is_complete(), "{report:?}");
    assert_eq!(harness.records(), payloads(&["a"]));
    assert_eq!(harness.stored_events().last(), Some(&"stopped"));
}

#[tokio::test(start_paused = true)]
async fn the_final_flush_retries_within_its_deadline_and_reports_the_rest() {
    let script = Script {
        writes: std::iter::repeat_n(fail(NO_ANSWER), 100).collect(),
        ..Script::default()
    };
    let mut harness = start(script, Limits::PRODUCTION);
    sleep(secs(5)).await;
    harness.push("a", 3);
    harness.push("b", 0);
    let started = Instant::now();
    let report = harness.shut_down(secs(20)).await;
    assert_eq!(started.elapsed(), secs(20));
    assert_eq!(
        report,
        Report {
            records: 2,
            signal_rows: 3,
            events: 0,
            events_not_kept: 0,
        }
    );
    // A fresh cadence: 0, 1, 3, 7, 15 s after the request.
    let writes: Vec<u64> = harness
        .writes()
        .iter()
        .map(|(at, _)| at.as_secs() - 5)
        .collect();
    assert_eq!(writes, [0, 1, 3, 7, 15]);
}

#[tokio::test(start_paused = true)]
async fn the_shutdown_cuts_a_call_that_hangs_and_the_final_flush_writes_again() {
    let limits = Limits {
        flush_rows: 1,
        ..Limits::PRODUCTION
    };
    let script = Script {
        writes: [Step::Hang].into(),
        ..Script::default()
    };
    let mut harness = start(script, limits);
    sleep(secs(1)).await;
    harness.push("a", 1);
    sleep(secs(30)).await;
    assert_eq!(harness.writes(), [(secs(1), 1)]);
    let report = harness.shut_down(secs(20)).await;
    assert!(report.is_complete(), "{report:?}");
    assert_eq!(harness.writes(), [(secs(1), 1), (secs(31), 1)]);
    assert_eq!(harness.records(), payloads(&["a"]));
}

#[tokio::test(start_paused = true)]
async fn the_final_flush_runs_the_check_first() {
    let script = Script {
        checks: std::iter::repeat_n(fail(NO_ANSWER), 3).collect(),
        ..Script::default()
    };
    let mut harness = start(script, Limits::PRODUCTION);
    sleep(Duration::from_millis(1_500)).await;
    harness.push("a", 1);
    let report = harness.shut_down(secs(20)).await;
    assert!(report.is_complete(), "{report:?}");
    let checks: Vec<u64> = harness
        .checks()
        .iter()
        .map(|at| at.as_millis().try_into().unwrap())
        .collect();
    // Two checks before the request, then the fresh cadence: 1,500 ms and
    // 2,500 ms.
    assert_eq!(checks, [0, 1_000, 1_500, 2_500]);
    assert_eq!(harness.records(), payloads(&["a"]));
}

// The gate conditions of the writer and the queue never stay false without
// a next event that can make them true (ADR 0001, "No permanent close").

/// Limits with a queue bound of 2.5 items without signal rows: two items
/// reach the high mark.
fn two_item_limits() -> Limits {
    let one = item("x", 0).bytes();
    Limits {
        queue_bytes: one * 2 + one / 2,
        ..Limits::PRODUCTION
    }
}

#[tokio::test(start_paused = true)]
async fn a_refused_last_record_does_not_keep_the_gate_closed() {
    // Review round 1 of PR #22, finding B1: a failed write, then the defect
    // case removes the last record.
    let script = Script {
        writes: [
            fail(NO_ANSWER),
            Step::Fail {
                error: data("22021"),
                committed: 1,
            },
        ]
        .into(),
        ..Script::default()
    };
    let mut harness = start(script, two_item_limits());
    harness.push("a", 0);
    harness.push("b", 0);
    sleep(secs(10)).await;
    assert!(harness.queue.is_empty());
    assert_eq!(harness.records(), payloads(&["a"]));
    sleep(Duration::from_secs(7200)).await;
    assert_eq!(harness.readiness.state().closed_reason(), None);
    harness.shut_down(secs(20)).await;
}

#[tokio::test(start_paused = true)]
async fn a_failed_write_that_committed_every_record_does_not_keep_the_gate_closed() {
    // A failure after the last commit: the queue is empty, and no write
    // follows.
    let script = Script {
        writes: [
            fail(NO_ANSWER),
            Step::Fail {
                error: NO_ANSWER,
                committed: 2,
            },
        ]
        .into(),
        ..Script::default()
    };
    let mut harness = start(script, two_item_limits());
    harness.push("a", 0);
    harness.push("b", 0);
    sleep(Duration::from_millis(500)).await;
    assert_eq!(
        harness.readiness.state().closed_reason(),
        Some("queue_high_mark")
    );
    sleep(secs(10)).await;
    assert!(harness.queue.is_empty());
    assert_eq!(harness.records(), payloads(&["a", "b"]));
    assert!(harness.readiness.state().is_healthy());
    harness.shut_down(secs(20)).await;
}

#[tokio::test(start_paused = true)]
async fn a_full_queue_in_degraded_mode_opens_the_gate_when_a_check_passes() {
    // The checks fail without an answer until 255 s (see
    // `no_answer_for_120_s_enters_degraded_mode_and_a_later_check_ends_it`).
    let script = Script {
        checks: std::iter::repeat_n(fail(NO_ANSWER), 8).collect(),
        ..Script::default()
    };
    let mut harness = start(script, two_item_limits());
    sleep(secs(130)).await;
    assert_eq!(harness.conditions.database(), Database::Degraded);
    harness.push("a", 0);
    harness.push("b", 0);
    sleep(secs(1)).await;
    // No write before a check has passed: the queue keeps the gate closed.
    assert_eq!(
        harness.readiness.state().closed_reason(),
        Some("queue_high_mark")
    );
    assert_eq!(harness.writes(), Vec::<(Duration, usize)>::new());
    sleep(secs(130)).await;
    assert_eq!(harness.writes(), [(secs(255), 2)]);
    assert!(harness.readiness.state().is_healthy());
    harness.shut_down(secs(20)).await;
}

#[tokio::test]
async fn the_database_store_passes_the_progress_hook_to_the_storage() {
    // Review round 1 of PR #22, mutation M10. The storage calls the hook
    // after the connect. The fake server answers the startup without TLS
    // and refuses each query, so each call ends at once with an answered
    // failure.
    use crate::storage::deadline_tests::{FakeServer, Mode};
    let server = FakeServer::start(Mode::Refuse);
    let mut store = DatabaseStore::new(server.options(), Uuid::nil());
    let record = item("a", 1);

    let mut beats = 0_u32;
    let failure = store
        .write(&[record.queued()], &mut || beats += 1)
        .await
        .unwrap_err();
    assert_eq!(failure.error.class, FailureClass::Answered);
    assert!(beats >= 1, "the write call did not run the hook");

    let mut beats = 0_u32;
    let error = store
        .write_events(&[IngestEvent::now(EventKind::Started)], &mut || {
            beats += 1;
        })
        .await
        .unwrap_err();
    assert_eq!(error.class, FailureClass::Answered);
    assert!(beats >= 1, "the events call did not run the hook");
}

/// Runs a writer with `script` and a watchdog with the writer limit
/// `limit` on the real clock (the watchdog thread measures real time) for
/// `run`, with one queued record. Returns true when the watchdog acted.
async fn watchdog_acts(script: Script, limit: Duration, run: Duration) -> bool {
    let task_limit = Task {
        name: "writer",
        limit,
    };
    let watchdog = Watchdog::new();
    let wake = Arc::new(Notify::new());
    let events = Arc::new(EventBuffer::new(10, Arc::clone(&wake)));
    let sink: Arc<dyn EventSink> = Arc::clone(&events) as Arc<dyn EventSink>;
    let readiness = Arc::new(Readiness::new(Arc::clone(&sink)));
    let conditions = Arc::new(Conditions::new(readiness, sink));
    let limits = Limits {
        flush_rows: 1,
        ..Limits::PRODUCTION
    };
    let queue = Arc::new(Queue::new(
        &limits,
        Arc::clone(&conditions),
        Arc::clone(&wake),
    ));
    assert!(queue.push(item("a", 1)));
    let writer = Writer {
        store: FakeStore {
            script: Arc::new(Mutex::new(script)),
            start: Instant::now(),
        },
        queue,
        events,
        conditions,
        wake,
        limits,
        flush_interval: INTERVAL,
        jitter: || 0.0,
    };
    let (_shutdown, requests) = oneshot::channel();
    let heartbeat = watchdog.register(task_limit);
    let (late, expired) = std::sync::mpsc::channel();
    watchdog
        .start(Duration::from_millis(50), move |name| {
            late.send(name).ok();
        })
        .unwrap();
    let task = tokio::spawn(writer.run(heartbeat, requests));
    sleep(run).await;
    task.abort();
    expired.try_recv().is_ok()
}

#[tokio::test]
async fn a_write_with_many_transactions_beats_before_each_one() {
    // Five transactions of 1 s: the call takes 5 s, the limit is 3 s.
    let script = Script {
        writes: [Step::Slow {
            count: 5,
            each: secs(1),
        }]
        .into(),
        ..Script::default()
    };
    assert!(!watchdog_acts(script, secs(3), Duration::from_millis(5_500)).await);
}

#[tokio::test]
async fn a_call_that_waits_past_the_limit_ends_the_writer_through_the_watchdog() {
    // A store without a deadline: the call never ends. The writer does not
    // beat during a call, so the watchdog acts.
    let script = Script {
        checks: [Step::Hang].into(),
        ..Script::default()
    };
    assert!(watchdog_acts(script, secs(1), secs(3)).await);
}
