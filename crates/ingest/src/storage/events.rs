//! The insert of `raw.ingest_event` rows (ADR 0002).
//!
//! [`write_events`] takes the events of the event sink
//! ([`crate::events::IngestEvent`]) as they are: the column `kind` is
//! [`EventKind::name`](crate::events::EventKind::name), `reason` is
//! [`EventKind::reason`](crate::events::EventKind::reason), and
//! `dropped_count`, `first_received_at`, and `last_received_at` come from
//! [`EventKind::window`](crate::events::EventKind::window). The count is a
//! `u32`; the column is `bigint`, so every count fits. The CHECK
//! constraints of the table hold the same rules as the types, so a
//! rejected event is a defect.

use std::time::Duration;

use jiff_sqlx::ToSqlx;
use sqlx::PgPool;
use uuid::Uuid;

use super::connect::{CallConnection, RUN_CALL_DEADLINE, within};
use super::error::{FailureClass, Phase, StorageError, classify};
use super::rows::floor_micros;
use crate::events::IngestEvent;

/// The counts of one call of [`write_events`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventOutcome {
    /// New rows.
    pub inserted: u64,
    /// Rows with a key `(run_id, kind, occurred_at)` that is stored
    /// already. Within one run, the same kind at the same microsecond is
    /// a retry of the same event (assumption: the process makes no two
    /// events of one kind within one microsecond).
    pub duplicates: u64,
    /// Events that the table rejects (SQLSTATE class 22 or 23), for
    /// example a time before 4713 BC, which `timestamptz` cannot hold. They
    /// cannot be stored; the caller logs the count and drops them.
    pub rejected: u64,
}

/// Inserts the events of the run `run_id` in one statement, with
/// [`RUN_CALL_DEADLINE`] on the client.
///
/// After a data error, the function inserts each event in a transaction
/// of its own, so one wrong event does not stop the others; it counts the
/// rejected events.
///
/// `progress` runs before each insert, so it runs at least once every
/// [`RUN_CALL_DEADLINE`] while the call lasts (the storage module
/// documentation, "Progress"). The pipeline beats the watchdog there.
///
/// # Errors
///
/// Returns the first failure that is not a data error. What is stored is
/// then not known exactly: an insert whose answer was lost can be
/// committed, and after a data error of the first insert, the events
/// before the failed single insert are stored. The caller keeps every
/// event and calls the function again; the insert is idempotent, so a
/// stored event becomes a duplicate.
pub async fn write_events(
    pool: &PgPool,
    run_id: Uuid,
    events: &[IngestEvent],
    progress: &mut (dyn FnMut() + Send),
) -> Result<EventOutcome, StorageError> {
    write_events_within(pool, run_id, events, progress, RUN_CALL_DEADLINE).await
}

/// [`write_events`] with another deadline for each insert, for the tests.
pub(crate) async fn write_events_within(
    pool: &PgPool,
    run_id: Uuid,
    events: &[IngestEvent],
    progress: &mut (dyn FnMut() + Send),
    deadline: Duration,
) -> Result<EventOutcome, StorageError> {
    progress();
    match insert(pool, run_id, events, deadline).await {
        Ok(inserted) => Ok(EventOutcome {
            inserted,
            duplicates: count(events.len()).saturating_sub(inserted),
            rejected: 0,
        }),
        Err(error) if error.class == FailureClass::Data && events.len() > 1 => {
            let mut outcome = EventOutcome::default();
            for event in events {
                progress();
                match insert(pool, run_id, std::slice::from_ref(event), deadline).await {
                    Ok(1) => outcome.inserted += 1,
                    Ok(_) => outcome.duplicates += 1,
                    Err(error) if error.class == FailureClass::Data => outcome.rejected += 1,
                    Err(error) => return Err(error),
                }
            }
            Ok(outcome)
        }
        Err(error) if error.class == FailureClass::Data => Ok(EventOutcome {
            rejected: count(events.len()),
            ..EventOutcome::default()
        }),
        Err(error) => Err(error),
    }
}

fn count(length: usize) -> u64 {
    u64::try_from(length).unwrap_or(u64::MAX)
}

async fn insert(
    pool: &PgPool,
    run_id: Uuid,
    events: &[IngestEvent],
    deadline: Duration,
) -> Result<u64, StorageError> {
    within(deadline, insert_now(pool, run_id, events)).await
}

async fn insert_now(
    pool: &PgPool,
    run_id: Uuid,
    events: &[IngestEvent],
) -> Result<u64, StorageError> {
    if events.is_empty() {
        return Ok(0);
    }
    let occurred_at: Vec<_> = events
        .iter()
        .map(|event| floor_micros(event.occurred_at).to_sqlx())
        .collect();
    let kind: Vec<&str> = events.iter().map(|event| event.kind.name()).collect();
    let reason: Vec<Option<String>> = events.iter().map(|event| event.kind.reason()).collect();
    let dropped_count: Vec<Option<i64>> = events
        .iter()
        .map(|event| event.kind.window().map(|window| i64::from(window.count)))
        .collect();
    let first: Vec<_> = events
        .iter()
        .map(|event| {
            event
                .kind
                .window()
                .map(|window| floor_micros(window.first).to_sqlx())
        })
        .collect();
    let last: Vec<_> = events
        .iter()
        .map(|event| {
            event
                .kind
                .window()
                .map(|window| floor_micros(window.last).to_sqlx())
        })
        .collect();
    // One statement is one implicit transaction.
    let mut call = CallConnection::acquire(pool).await?;
    let result = sqlx::query!(
        r#"INSERT INTO ingest_event (
               run_id, occurred_at, kind, reason, dropped_count, first_received_at, last_received_at
           )
           SELECT $1::uuid, occurred_at, kind, reason, dropped_count, first_received_at, last_received_at
           FROM UNNEST(
               $2::timestamptz[], $3::text[], $4::text[], $5::bigint[], $6::timestamptz[],
               $7::timestamptz[]
           ) AS e (occurred_at, kind, reason, dropped_count, first_received_at, last_received_at)
           ON CONFLICT DO NOTHING"#,
        run_id,
        &occurred_at as _,
        &kind as _,
        &reason as _,
        &dropped_count as _,
        &first as _,
        &last as _,
    )
    .execute(call.get()?)
    .await
    .map_err(|error| classify(Phase::Statement, &error));
    call.finish(
        result
            .as_ref()
            .map_or_else(|error| error.answered(), |_| true),
    );
    Ok(result?.rows_affected())
}
