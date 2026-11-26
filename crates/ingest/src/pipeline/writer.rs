//! The writer: the start-up check, degraded mode, the flushes, the retry
//! cadence, and the final flush (ADR 0001, gate condition 3, "Delivery
//! path", steps 4 to 6, and "Shutdown", step 5).
//!
//! # Start-up check and degraded mode
//!
//! The writer runs [`Store::check`] (one round trip: the embedded
//! migrations are applied) at its start, and again on the retry cadence
//! until it passes. Gate condition 3 follows the result ([`Database`]):
//!
//! | Result | Condition |
//! | ------ | --------- |
//! | Passed | `Passed`. The writer flushes at once, so the events of the start reach the database while the compute is awake. |
//! | No answer ([`super::cadence::is_no_answer`]) | `Pending`. After [`Limits::degraded_after`] (120 s) of no answer without an answered failure between, `Degraded`: the gate opens, the records wait in the queue, and the `degraded_mode` event waits with them. |
//! | Answered failure | `Refused`: the gate closes at once, also in degraded mode. The queue keeps its records. |
//!
//! The writer writes no record before a check has passed, so in degraded
//! mode the check runs before the first write (ADR 0001).
//!
//! # Flush
//!
//! After the check, the writer flushes when the oldest item of the queue or
//! of the event buffer is [`Writer::flush_interval`] old (900 s), when
//! [`Limits::flush_rows`] (5,000) signal rows wait, or when the queue has
//! closed the gate at its high mark. The last trigger is not in ADR 0001:
//! without it, a burst of records without signal rows could keep the gate
//! closed for up to one flush interval (ADR 0001 records the decision).
//!
//! A flush writes the queue from the front in batches of one transaction:
//! at most [`MAX_SIGNAL_ROWS_PER_TRANSACTION`] signal rows and
//! [`MAX_BYTES_PER_TRANSACTION`] bytes of messages. After each committed
//! batch the items leave the queue. Then the flush writes the waiting
//! events with the run ID.
//!
//! # Failures
//!
//! The storage caller contract (`crate::storage`) is binding:
//!
//! - On a failure, the records `records[..committed]` are stored and leave
//!   the queue; the rest stay.
//! - A failure of class `Data` is the defect case: the database refused
//!   even the `unprocessed_record` row of the record `records[committed]`.
//!   The writer logs the code, removes that record, records a
//!   `records_dropped` event with the reason `database_refused`, and goes
//!   on at once. A retry would fail the same way without end.
//! - Every other failure keeps the records and waits for the retry cadence
//!   ([`Cadence`]). It also clears the "last write succeeded" condition of
//!   the low mark. When the failure leaves the queue empty, the queue opens
//!   the gate anyway: no record waits, so no retry follows that could open
//!   it (ADR 0001, "No permanent close").
//! - **Shrink.** A statement timeout (SQLSTATE `57014`) or a client
//!   deadline (`timeout`) would repeat without end for the same batch: three
//!   equal answered failures park the writer at 60 minutes. After such a
//!   failure the next batch has half the signal rows and half the bytes,
//!   down to one record; a successful batch restores the full size. A
//!   single record that still times out stays on the cadence.
//!
//! # Heartbeat
//!
//! The writer beats each second while it waits for a trigger or for a
//! retry, and before each database call. Inside a call, the storage calls
//! the progress hook after the connect and before each transaction
//! ([`Store::write`]), and the hook beats, because one write call can hold
//! many transactions when a data error splits a batch. The storage bounds
//! each wait without a hook call: 15 s for the connect, 90 s between two
//! hook calls, 95 s after the last one, and 90 s for the check (`storage`
//! module documentation). All are below the 120 s of the watchdog task
//! `writer`. So a live writer beats within 120 s, and a call that waits
//! past every deadline, or a writer that is not polled, ends the process
//! through the watchdog (ADR 0001, "Watchdog").
//!
//! # Final flush
//!
//! At the shutdown ([`ShutdownRequest`]) the writer stops the current wait
//! or database call. A call that the stop cuts can have committed: its
//! records stay in the queue, and the retry stores nothing twice. Then the
//! writer flushes the queue and every event, the `stopped` event too, with
//! a fresh cadence and within the deadline of the request. A check that has
//! not passed runs first. The report tells what is not stored.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::PgConnectOptions;
use tokio::sync::{Notify, oneshot};
use tokio::time::{Instant, Interval, MissedTickBehavior, sleep_until};
use tracing::{error, info, warn};
use uuid::Uuid;

use super::Limits;
use super::cadence::{Cadence, is_no_answer};
use super::conditions::{Conditions, Database};
use super::queue::Queue;
use crate::events::{
    CountWindow, DROPPED_DATABASE_REFUSED, EventBuffer, EventKind, EventSink, IngestEvent,
};
use crate::storage::{
    self, ErrorCode, EventOutcome, FailureClass, MAX_BYTES_PER_TRANSACTION,
    MAX_SIGNAL_ROWS_PER_TRANSACTION, QueuedRecord, StorageError, WriteError, WriteOutcome,
};
use crate::watchdog::{BEAT_INTERVAL, Heartbeat};

/// The database calls of the writer. [`Database`](DatabaseStore) is the
/// production store; the tests use a store in memory.
pub trait Store: Send {
    /// The start-up check ([`storage::check_migrations`]).
    fn check(&mut self) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Writes one batch ([`storage::Writer::write`]). The store calls
    /// `progress` before each transaction, so that a call with many
    /// transactions beats the watchdog.
    fn write(
        &mut self,
        records: &[QueuedRecord<'_>],
        progress: &mut (dyn FnMut() + Send),
    ) -> impl Future<Output = Result<WriteOutcome, WriteError>> + Send;

    /// Writes events with the run ID ([`storage::write_events`]). The store
    /// calls `progress` before each statement.
    fn write_events(
        &mut self,
        events: &[IngestEvent],
        progress: &mut (dyn FnMut() + Send),
    ) -> impl Future<Output = Result<EventOutcome, StorageError>> + Send;
}

/// The production store: the storage writer of `ingest run`, the connection
/// settings, and the run ID of this process (ADR 0002,
/// `raw.ingest_event.run_id`).
///
/// It is the only place that calls the storage. Each storage call opens one
/// connection and closes it before it returns, and the writer makes one
/// call at a time, so the process holds at most one database connection;
/// the role `telemetry_ingest` allows 5 (ADR 0002).
///
/// `PgConnectOptions` has a `Debug` that shows the password, so this type
/// has no `Debug`.
pub struct DatabaseStore {
    writer: storage::Writer,
    options: PgConnectOptions,
    run_id: Uuid,
}

impl DatabaseStore {
    /// A store with the connection settings `options`
    /// ([`storage::connect_options`]) for the run `run_id`. It opens no
    /// connection before the first call.
    #[must_use]
    pub fn new(options: PgConnectOptions, run_id: Uuid) -> Self {
        Self {
            writer: storage::Writer::new(options.clone()),
            options,
            run_id,
        }
    }
}

impl Store for DatabaseStore {
    async fn check(&mut self) -> Result<(), StorageError> {
        storage::check_migrations(&self.options).await
    }

    async fn write(
        &mut self,
        records: &[QueuedRecord<'_>],
        progress: &mut (dyn FnMut() + Send),
    ) -> Result<WriteOutcome, WriteError> {
        self.writer.write(records, progress).await
    }

    async fn write_events(
        &mut self,
        events: &[IngestEvent],
        progress: &mut (dyn FnMut() + Send),
    ) -> Result<EventOutcome, StorageError> {
        storage::write_events(&self.options, self.run_id, events, progress).await
    }
}

/// The request of the final flush.
#[derive(Debug)]
pub struct ShutdownRequest {
    /// The bound of the final flush (ADR 0001: 20 s).
    pub deadline: Duration,
    /// The report of what is not stored.
    pub done: oneshot::Sender<Report>,
}

/// What the final flush did not store. Numbers only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    /// Records (messages) that stay in the queue.
    pub records: usize,
    /// Their signal rows.
    pub signal_rows: usize,
    /// Events that wait for the writer.
    pub events: usize,
    /// Events that the full event buffer did not keep during the run.
    pub events_not_kept: u64,
}

impl Report {
    /// True when everything is stored.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.records == 0 && self.events == 0 && self.events_not_kept == 0
    }
}

/// The size limit of the next batch (module documentation, "Shrink").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BatchLimit {
    rows: usize,
    bytes: usize,
}

impl BatchLimit {
    const FULL: Self = Self {
        rows: MAX_SIGNAL_ROWS_PER_TRANSACTION,
        bytes: MAX_BYTES_PER_TRANSACTION,
    };

    fn shrink(&mut self) {
        self.rows = (self.rows / 2).max(1);
        self.bytes = (self.bytes / 2).max(1);
    }
}

/// True for the failures that a smaller batch can avoid.
fn shrinks(error: StorageError) -> bool {
    error.code == ErrorCode::Timeout
        || error
            .sqlstate()
            .is_some_and(|state| state.as_str() == "57014")
}

/// The outcome of one attempt.
enum Attempt {
    /// Everything that the attempt had to do is done.
    Done,
    /// A failure: the next attempt waits this delay.
    Failed(Duration),
    /// The shutdown asked for the final flush during the attempt.
    Shutdown(Option<ShutdownRequest>),
}

/// The state of the writer between attempts.
struct State {
    /// The time of the next attempt of the check, or of the retry of a
    /// failed write.
    retry_at: Option<Instant>,
    /// The start of the current time of no answer of the check.
    no_answer_since: Option<Instant>,
    limit: BatchLimit,
    cadence: Cadence,
}

/// The sums of one flush, for the log.
#[derive(Debug, Default)]
struct Written {
    records: usize,
    outcome: WriteOutcome,
    events: EventOutcome,
}

/// The writer task.
pub struct Writer<S> {
    /// The database.
    pub store: S,
    /// The queue.
    pub queue: Arc<Queue>,
    /// The events that wait for the writer.
    pub events: Arc<EventBuffer>,
    /// The owner of gate condition 3.
    pub conditions: Arc<Conditions>,
    /// The wake of the writer: the queue and the event buffer notify it.
    pub wake: Arc<Notify>,
    /// The bounds.
    pub limits: Limits,
    /// The flush interval (`INGEST_FLUSH_INTERVAL_SECONDS`).
    pub flush_interval: Duration,
    /// The jitter source of the cadence: a fraction from 0 to 1.
    pub jitter: fn() -> f64,
}

impl<S: Store> Writer<S> {
    /// Runs the writer until `shutdown` asks for the final flush, then
    /// flushes and ends. A dropped sender ends the writer without a flush.
    pub async fn run(
        mut self,
        heartbeat: Heartbeat,
        mut shutdown: oneshot::Receiver<ShutdownRequest>,
    ) {
        let mut tick = tokio::time::interval(BEAT_INTERVAL);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut state = State {
            retry_at: Some(Instant::now()),
            no_answer_since: None,
            limit: BatchLimit::FULL,
            cadence: Cadence::new(&self.limits),
        };
        let request = loop {
            heartbeat.beat();
            let action_at = self.next_action(&state);
            let degraded_at = self.degraded_at(&state);
            tokio::select! {
                () = sleep_until(action_at.unwrap_or_else(Instant::now)), if action_at.is_some() => {
                    match self.attempt(&mut state, &heartbeat, Some(&mut shutdown)).await {
                        Attempt::Done => state.retry_at = None,
                        Attempt::Failed(delay) => state.retry_at = Some(Instant::now() + delay),
                        Attempt::Shutdown(request) => break request,
                    }
                }
                () = sleep_until(degraded_at.unwrap_or_else(Instant::now)), if degraded_at.is_some() => {
                    enter_degraded(&self.conditions);
                }
                () = self.wake.notified() => {}
                _ = tick.tick() => {}
                request = &mut shutdown => break request.ok(),
            }
        };
        if let Some(request) = request {
            let report = self
                .final_flush(request.deadline, &heartbeat, &mut tick)
                .await;
            request.done.send(report).ok();
        }
        heartbeat.retire();
    }

    /// The time of the next attempt (module documentation).
    fn next_action(&self, state: &State) -> Option<Instant> {
        if self.conditions.database() != Database::Passed {
            return state.retry_at;
        }
        let pending = !self.queue.is_empty() || !self.events.is_empty();
        if !pending {
            return None;
        }
        if state.retry_at.is_some() {
            return state.retry_at;
        }
        if self.queue.wants_flush() {
            return Some(Instant::now());
        }
        let oldest = match (self.queue.oldest(), self.events.oldest()) {
            (Some(queue), Some(events)) => queue.min(events),
            (queue, events) => queue.or(events)?,
        };
        Some(oldest + self.flush_interval)
    }

    /// The time when degraded mode starts, while the check waits for an
    /// answer.
    fn degraded_at(&self, state: &State) -> Option<Instant> {
        if self.conditions.database() != Database::Pending {
            return None;
        }
        state
            .no_answer_since
            .map(|since| since + self.limits.degraded_after)
    }

    /// One attempt: the check when it has not passed, else a flush.
    async fn attempt(
        &mut self,
        state: &mut State,
        heartbeat: &Heartbeat,
        shutdown: Option<&mut oneshot::Receiver<ShutdownRequest>>,
    ) -> Attempt {
        if self.conditions.database() == Database::Passed {
            return self.flush(state, heartbeat, shutdown).await;
        }
        let started = Instant::now();
        let mut shutdown = shutdown;
        // The time of no answer starts with the first check that has no
        // answer yet. Such a check can wait up to the connect timeout of
        // 15 s, and the limit of degraded mode can pass during that wait.
        if self.conditions.database() == Database::Pending {
            state.no_answer_since.get_or_insert(started);
        }
        let degraded_at = self.degraded_at(state);
        let store = &mut self.store;
        let conditions = &self.conditions;
        let check = async move {
            let check = store.check();
            tokio::pin!(check);
            if let Some(at) = degraded_at {
                tokio::select! {
                    result = &mut check => return result,
                    () = sleep_until(at) => enter_degraded(conditions),
                }
            }
            check.await
        };
        let result = match call(check, heartbeat, shutdown.as_deref_mut()).await {
            Ok(result) => result,
            Err(request) => return Attempt::Shutdown(request),
        };
        match result {
            Ok(()) => {
                info!("database_check_passed");
                self.conditions.set_database(Database::Passed);
                state.cadence.reset();
                state.no_answer_since = None;
                // The flush follows at once, while the compute is awake.
                self.flush(state, heartbeat, shutdown).await
            }
            Err(failure) => {
                let delay = state.cadence.failed(&failure, (self.jitter)());
                if is_no_answer(&failure) {
                    warn!(class = failure.class.as_str(), code = %failure.code, delay_ms = millis(delay), "database_check_failed");
                    state.no_answer_since.get_or_insert(started);
                    if self.conditions.database() == Database::Refused {
                        self.conditions.set_database(Database::Pending);
                    }
                } else {
                    error!(class = failure.class.as_str(), code = %failure.code, delay_ms = millis(delay), "database_check_failed");
                    state.no_answer_since = None;
                    self.conditions.set_database(Database::Refused);
                }
                Attempt::Failed(delay)
            }
        }
    }

    /// Writes the queue and then the events (module documentation).
    async fn flush(
        &mut self,
        state: &mut State,
        heartbeat: &Heartbeat,
        mut shutdown: Option<&mut oneshot::Receiver<ShutdownRequest>>,
    ) -> Attempt {
        let mut written = Written::default();
        loop {
            let batch = self.queue.batch(state.limit.rows, state.limit.bytes);
            if batch.is_empty() {
                break;
            }
            let records: Vec<QueuedRecord<'_>> = batch.iter().map(|item| item.queued()).collect();
            let mut progress = || heartbeat.beat();
            let result = match call(
                self.store.write(&records, &mut progress),
                heartbeat,
                shutdown.as_deref_mut(),
            )
            .await
            {
                Ok(result) => result,
                Err(request) => return Attempt::Shutdown(request),
            };
            match result {
                Ok(outcome) => {
                    self.queue.remove(batch.len());
                    self.queue.set_last_write_ok(true);
                    state.cadence.reset();
                    state.limit = BatchLimit::FULL;
                    written.records += batch.len();
                    add(&mut written.outcome, outcome);
                }
                Err(failure) => {
                    let committed = failure.committed.min(batch.len());
                    self.queue.remove(committed);
                    written.records += committed;
                    add(&mut written.outcome, failure.outcome);
                    if failure.error.class == FailureClass::Data
                        && let Some(item) = batch.get(committed)
                    {
                        // The defect case of the storage contract.
                        error!(class = failure.error.class.as_str(), code = %failure.error.code, count = 1_u32, "record_refused");
                        self.events
                            .record(IngestEvent::now(EventKind::RecordsDropped {
                                window: CountWindow::one(item.received_at()),
                                reason: DROPPED_DATABASE_REFUSED,
                            }));
                        self.queue.remove(1);
                        continue;
                    }
                    if committed > 0 {
                        // Progress: see `cadence`.
                        state.cadence.reset();
                    }
                    if shrinks(failure.error) {
                        state.limit.shrink();
                    }
                    self.queue.set_last_write_ok(false);
                    let delay = state.cadence.failed(&failure.error, (self.jitter)());
                    // The class and the code are static text or a SQLSTATE;
                    // the text of the database error is never kept (ADR 0001,
                    // "Logging").
                    warn!(
                        class = failure.error.class.as_str(),
                        code = %failure.error.code,
                        records = committed,
                        delay_ms = millis(delay),
                        "write_failed"
                    );
                    log_written(&written);
                    return Attempt::Failed(delay);
                }
            }
        }
        let done = self
            .flush_events(state, heartbeat, shutdown, &mut written)
            .await;
        log_written(&written);
        done
    }

    /// Writes the waiting events with the run ID.
    async fn flush_events(
        &mut self,
        state: &mut State,
        heartbeat: &Heartbeat,
        shutdown: Option<&mut oneshot::Receiver<ShutdownRequest>>,
        written: &mut Written,
    ) -> Attempt {
        let events = self.events.pending();
        if !events.is_empty() {
            let mut progress = || heartbeat.beat();
            let result = match call(
                self.store.write_events(&events, &mut progress),
                heartbeat,
                shutdown,
            )
            .await
            {
                Ok(result) => result,
                Err(request) => return Attempt::Shutdown(request),
            };
            match result {
                Ok(outcome) => {
                    self.events.remove(events.len());
                    state.cadence.reset();
                    written.events = outcome;
                    if outcome.rejected > 0 {
                        error!(count = outcome.rejected, "events_rejected");
                    }
                }
                Err(failure) => {
                    let delay = state.cadence.failed(&failure, (self.jitter)());
                    warn!(
                        class = failure.class.as_str(),
                        code = %failure.code,
                        delay_ms = millis(delay),
                        "events_write_failed"
                    );
                    return Attempt::Failed(delay);
                }
            }
        }
        Attempt::Done
    }

    /// The final flush within `deadline` (module documentation).
    async fn final_flush(
        &mut self,
        deadline: Duration,
        heartbeat: &Heartbeat,
        tick: &mut Interval,
    ) -> Report {
        let mut state = State {
            retry_at: None,
            no_answer_since: None,
            limit: BatchLimit::FULL,
            cadence: Cadence::new(&self.limits),
        };
        let flushed = tokio::time::timeout(deadline, async {
            loop {
                match self.attempt(&mut state, heartbeat, None).await {
                    Attempt::Done => return,
                    Attempt::Failed(delay) => {
                        let until = Instant::now() + delay;
                        loop {
                            tokio::select! {
                                () = sleep_until(until) => break,
                                _ = tick.tick() => heartbeat.beat(),
                            }
                        }
                    }
                    Attempt::Shutdown(_) => {}
                }
            }
        })
        .await;
        if flushed.is_err() {
            warn!("final_flush_deadline");
        }
        let left = self.queue.stats();
        Report {
            records: left.items,
            signal_rows: left.signal_rows,
            events: self.events.len(),
            events_not_kept: self.events.not_kept(),
        }
    }
}

/// Runs a database call. The writer beats once before the call; inside
/// the call, the store beats before each transaction (module
/// documentation, "Heartbeat"). A shutdown request ends the wait and drops
/// the call.
async fn call<T>(
    work: impl Future<Output = T>,
    heartbeat: &Heartbeat,
    shutdown: Option<&mut oneshot::Receiver<ShutdownRequest>>,
) -> Result<T, Option<ShutdownRequest>> {
    heartbeat.beat();
    tokio::pin!(work);
    let mut shutdown = shutdown;
    tokio::select! {
        result = &mut work => Ok(result),
        request = async { shutdown.as_deref_mut()?.await.ok() }, if shutdown.is_some() => Err(request),
    }
}

/// The limit of no answer has passed (gate condition 3).
fn enter_degraded(conditions: &Conditions) {
    if conditions.database() == Database::Pending {
        warn!("degraded_mode");
        conditions.set_database(Database::Degraded);
    }
}

fn add(sum: &mut WriteOutcome, outcome: WriteOutcome) {
    sum.transactions += outcome.transactions;
    sum.rows_inserted += outcome.rows_inserted;
    sum.duplicate_rows += outcome.duplicate_rows;
    sum.conflicting_records += outcome.conflicting_records;
    sum.rejected_records += outcome.rejected_records;
    sum.unprocessed_inserted += outcome.unprocessed_inserted;
    sum.attempts += outcome.attempts;
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn log_written(written: &Written) {
    if written.records == 0 && written.events.inserted == 0 && written.events.duplicates == 0 {
        return;
    }
    info!(
        records = written.records,
        rows = written.outcome.rows_inserted,
        duplicates = written.outcome.duplicate_rows,
        conflicts = written.outcome.conflicting_records,
        rejected = written.outcome.rejected_records,
        unprocessed = written.outcome.unprocessed_inserted,
        events = written.events.inserted,
        "flushed"
    );
}

#[cfg(test)]
mod tests;
