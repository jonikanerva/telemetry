//! The batch writer (ADR 0002, "Write batching" and "Idempotency").

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;
use std::time::Duration;

use jiff_sqlx::ToSqlx;
use sqlx::{Connection, PgConnection, PgPool, Postgres, Transaction};

use super::connect::{CallConnection, RUN_CALL_DEADLINE, within};
use super::error::{ErrorCode, FailureClass, Phase, SqlState, StorageError, classify};
use super::rows::{
    ConnectivityKey, ConnectivityValue, Plan, QueuedRecord, Reason, SignalKey, SignalValueColumns,
    UnprocessedRow, VehicleId, compare_stored, floor_micros, vins_with_rows,
};

/// The most signal rows in one transaction (ADR 0002, "Write batching").
pub const MAX_SIGNAL_ROWS_PER_TRANSACTION: usize = 5_000;

/// The most bytes of the original messages (topic and payload) in one
/// transaction: 8 MiB. Records without signal rows (alerts, errors,
/// connectivity, unprocessed messages) do not count against
/// [`MAX_SIGNAL_ROWS_PER_TRANSACTION`], so this bound keeps every
/// transaction small enough for the statement timeout. Measured: 8 MB of
/// unprocessed records in one transaction took about 6 s on the test
/// branch.
pub const MAX_BYTES_PER_TRANSACTION: usize = 8 * 1024 * 1024;

/// The longest local work between two calls of the progress hook outside
/// of a call deadline: the bookkeeping between two transactions, which
/// takes microseconds. The rest of the margin is for the beat of the
/// pipeline and the check period of the watchdog (1 s).
const PROGRESS_MARGIN: Duration = Duration::from_secs(10);

// The progress guarantee of `Writer::write` (storage module documentation,
// "Progress"): two calls of the hook are at most one call deadline and the
// local work apart, and that must stay below the writer limit of the
// watchdog. A larger deadline or a smaller watchdog limit stops the build.
const _: () = assert!(
    RUN_CALL_DEADLINE.as_secs() + PROGRESS_MARGIN.as_secs()
        <= crate::watchdog::WRITER.limit.as_secs(),
    "RUN_CALL_DEADLINE leaves no margin below the writer limit of the watchdog"
);

/// The counts of one call of [`Writer::write`]. They hold numbers only, so
/// a log event may show them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteOutcome {
    /// Transaction attempts: the committed ones, the failed ones of a
    /// split, and the inserts of rejected records. Each attempt called the
    /// progress hook once. N records that the database rejects need about
    /// 2N - 1 failed attempts and N to 2N reject attempts (storage module
    /// documentation, "Cost of rejected records").
    pub attempts: u64,
    /// Committed transactions, also those of a split and of rejected
    /// records. Failed attempts are not counted here.
    pub transactions: u64,
    /// New rows in `vehicle_signal`, `vehicle_alert`, `vehicle_error`, and
    /// `connectivity_event`.
    pub rows_inserted: u64,
    /// Rows that were not inserted because an equal row exists: an equal
    /// copy earlier in the batch, an equal stored row, or a replay of an
    /// alert or an error item.
    pub duplicate_rows: u64,
    /// Records with a key that holds another value. Each one is stored in
    /// `unprocessed_record` with reason `conflicting_duplicate`; its other
    /// rows are stored.
    pub conflicting_records: u64,
    /// Records that the database rejected with a data error. Each one is
    /// stored in `unprocessed_record` with reason `database_rejected` and
    /// the SQLSTATE; none of its rows are stored.
    pub rejected_records: u64,
    /// New rows in `unprocessed_record`, for every reason.
    pub unprocessed_inserted: u64,
}

impl WriteOutcome {
    fn add(&mut self, other: Self) {
        self.attempts += other.attempts;
        self.transactions += other.transactions;
        self.rows_inserted += other.rows_inserted;
        self.duplicate_rows += other.duplicate_rows;
        self.conflicting_records += other.conflicting_records;
        self.rejected_records += other.rejected_records;
        self.unprocessed_inserted += other.unprocessed_inserted;
    }
}

/// A failed call of [`Writer::write`].
///
/// The records `records[..committed]` are stored: their rows, or their
/// `unprocessed_record` rows. The records `records[committed..]` have no
/// known commit. When the connection breaks during a `COMMIT`, the client
/// cannot know whether the server committed. The whole transaction can be
/// committed: the records from `committed` to the end of its part, up to
/// one full transaction of [`MAX_SIGNAL_ROWS_PER_TRANSACTION`] signal rows
/// or [`MAX_BYTES_PER_TRANSACTION`] bytes. The caller keeps
/// `records[committed..]` and writes them again. Every insert is
/// idempotent, so the second call stores nothing twice. The text holds the
/// class, the code, and a number only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{error} after {committed} stored records")]
pub struct WriteError {
    /// The failure.
    pub error: StorageError,
    /// The number of records at the start of the batch that are stored.
    pub committed: usize,
    /// The counts of the call until the failure: every attempt, also the
    /// failed one, and the counts of the committed transactions.
    pub outcome: WriteOutcome,
}

/// Writes batches of queued records to the raw tables.
///
/// The writer keeps the vehicle IDs that it has read or created, so a
/// flush needs no vehicle statement after the first one. It holds no
/// connection between two calls; the pool closes idle connections.
#[derive(Debug)]
pub struct Writer {
    pool: PgPool,
    vehicles: HashMap<String, VehicleId>,
    call_deadline: Duration,
}

/// The failure of one transaction.
enum AttemptError {
    /// A data error (SQLSTATE class 22 or 23): split the records.
    Data(SqlState),
    /// Any other failure: the caller retries the batch.
    Other(StorageError),
}

impl AttemptError {
    /// True when the server answered (`CallConnection::finish`).
    const fn answered(&self) -> bool {
        match self {
            Self::Data(_) => true,
            Self::Other(error) => error.answered(),
        }
    }
}

impl From<StorageError> for AttemptError {
    fn from(error: StorageError) -> Self {
        match (error.class, error.sqlstate()) {
            (FailureClass::Data, Some(state)) => Self::Data(state),
            _ => Self::Other(error),
        }
    }
}

fn statement(error: &sqlx::Error) -> StorageError {
    classify(Phase::Statement, error)
}

/// The progress hook of one call and its count of transaction attempts.
struct Progress<'p> {
    hook: &'p mut (dyn FnMut() + Send),
    attempts: u64,
}

impl Progress<'_> {
    /// Runs before every transaction attempt.
    fn attempt(&mut self) {
        self.attempts += 1;
        (self.hook)();
    }
}

impl Writer {
    /// Makes a writer that uses `pool` ([`super::run_pool`]).
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            vehicles: HashMap::new(),
            call_deadline: RUN_CALL_DEADLINE,
        }
    }

    /// The writer with another deadline for each transaction, for the
    /// tests.
    #[cfg(test)]
    pub(crate) fn with_call_deadline(mut self, deadline: Duration) -> Self {
        self.call_deadline = deadline;
        self
    }

    /// The pool of the writer.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Writes the records, in their order, and returns the counts.
    ///
    /// - The records go into transactions of at most
    ///   [`MAX_SIGNAL_ROWS_PER_TRANSACTION`] signal rows and
    ///   [`MAX_BYTES_PER_TRANSACTION`] bytes of messages; a record is never
    ///   split, so a larger record is a transaction of its own. Each
    ///   transaction has one statement for each table with rows, and reads
    ///   stored rows only for keys that it could not insert. Each
    ///   transaction has [`RUN_CALL_DEADLINE`] on the client.
    /// - Every insert is `ON CONFLICT DO NOTHING`, so a second call with the
    ///   same records is safe and stores nothing twice. The first stored
    ///   copy of a key wins. A copy with another value makes its record a
    ///   conflict ([`WriteOutcome::conflicting_records`]).
    /// - After a data error (SQLSTATE class 22 or 23), the writer splits the
    ///   records of that transaction into halves, each in its own
    ///   transaction, the first half first, until it has the one record
    ///   that fails. That record goes to `unprocessed_record` with reason
    ///   `database_rejected` in a transaction of its own. The other records
    ///   commit.
    /// - `progress` runs before every transaction attempt: each transaction
    ///   of the batch, each attempt of a split, and each insert of a
    ///   rejected record. Two runs of `progress` are therefore at most one
    ///   [`RUN_CALL_DEADLINE`] and some microseconds of local work apart,
    ///   also when one call of `write` lasts many minutes. The pipeline
    ///   beats the watchdog there (storage module documentation,
    ///   "Progress").
    ///
    /// # Errors
    ///
    /// Returns a [`WriteError`] at the first failure that is not a data
    /// error of a record: the failure, the number of records at the start
    /// of the batch that are stored, and the counts so far. The storage
    /// module documentation says how the caller treats each class.
    pub async fn write(
        &mut self,
        records: &[QueuedRecord<'_>],
        progress: &mut (dyn FnMut() + Send),
    ) -> Result<WriteOutcome, WriteError> {
        let mut progress = Progress {
            hook: progress,
            attempts: 0,
        };
        let mut outcome = WriteOutcome::default();
        for chunk in chunks(records) {
            if let Err((error, committed)) = self
                .write_isolating(records, chunk, &mut outcome, &mut progress)
                .await
            {
                outcome.attempts = progress.attempts;
                return Err(WriteError {
                    error,
                    committed,
                    outcome,
                });
            }
        }
        outcome.attempts = progress.attempts;
        Ok(outcome)
    }

    /// Writes `records[range]`. On a failure, returns it with the index of
    /// the first record that is not stored.
    async fn write_isolating(
        &mut self,
        records: &[QueuedRecord<'_>],
        range: Range<usize>,
        outcome: &mut WriteOutcome,
        progress: &mut Progress<'_>,
    ) -> Result<(), (StorageError, usize)> {
        // A stack of parts. The second half goes on first, so the parts
        // run in the order of the batch and the first copy of a key wins.
        let mut parts = vec![range];
        while let Some(part) = parts.pop() {
            let start = part.start;
            let slice = records.get(part.clone()).unwrap_or_default();
            if slice.is_empty() {
                continue;
            }
            progress.attempt();
            match self.attempt(slice).await {
                Ok(done) => outcome.add(done),
                Err(AttemptError::Other(error)) => return Err((error, start)),
                Err(AttemptError::Data(state)) => {
                    if let [record] = slice {
                        let rejected = self
                            .reject(record, state, progress)
                            .await
                            .map_err(|error| (error, start))?;
                        outcome.add(rejected);
                    } else {
                        let middle = start + slice.len() / 2;
                        parts.push(middle..part.end);
                        parts.push(start..middle);
                    }
                }
            }
        }
        Ok(())
    }

    /// Writes `records` in one transaction, with the call deadline.
    async fn attempt(
        &mut self,
        records: &[QueuedRecord<'_>],
    ) -> Result<WriteOutcome, AttemptError> {
        tokio::time::timeout(self.call_deadline, self.attempt_now(records))
            .await
            .unwrap_or(Err(AttemptError::Other(StorageError::new(
                FailureClass::NoAnswer,
                ErrorCode::Timeout,
            ))))
    }

    async fn attempt_now(
        &mut self,
        records: &[QueuedRecord<'_>],
    ) -> Result<WriteOutcome, AttemptError> {
        let mut call = CallConnection::acquire(&self.pool).await?;
        let result = self.attempt_on(call.get()?, records).await;
        call.finish(
            result
                .as_ref()
                .map_or_else(AttemptError::answered, |_| true),
        );
        let (outcome, new_vehicles) = result?;
        // Keep the IDs only after the commit: a rolled-back insert has no
        // row.
        self.vehicles.extend(new_vehicles);
        Ok(outcome)
    }

    async fn attempt_on(
        &self,
        conn: &mut PgConnection,
        records: &[QueuedRecord<'_>],
    ) -> Result<(WriteOutcome, HashMap<String, VehicleId>), AttemptError> {
        let mut tx = conn.begin().await.map_err(|error| statement(&error))?;
        match self.attempt_in(&mut tx, records).await {
            Ok(done) => {
                tx.commit().await.map_err(|error| statement(&error))?;
                Ok(done)
            }
            Err(error) => {
                // The rollback ends the failed transaction; when the
                // connection is broken, the server ends it anyway.
                let _rolled_back = tx.rollback().await;
                Err(error)
            }
        }
    }

    async fn attempt_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        records: &[QueuedRecord<'_>],
    ) -> Result<(WriteOutcome, HashMap<String, VehicleId>), AttemptError> {
        let mut vehicles = self.vehicles.clone();
        let mut new_vehicles = HashMap::new();
        for (vin, received_at) in vins_with_rows(records) {
            if !vehicles.contains_key(vin) {
                let id = vehicle_id(tx, vin, received_at).await?;
                vehicles.insert(vin.to_owned(), id);
                new_vehicles.insert(vin.to_owned(), id);
            }
        }
        let plan = Plan::build(records, &vehicles).map_err(|_| {
            AttemptError::Other(StorageError::new(FailureClass::Answered, ErrorCode::Other))
        })?;

        let mut outcome = WriteOutcome {
            transactions: 1,
            duplicate_rows: plan.duplicates,
            ..WriteOutcome::default()
        };
        let mut conflicting = plan.conflicting.clone();

        let (inserted, duplicates, conflicts) = insert_signals(tx, &plan).await?;
        outcome.rows_inserted += inserted;
        outcome.duplicate_rows += duplicates;
        conflicting.extend(conflicts);

        let (inserted, duplicates, conflicts) = insert_connectivity(tx, &plan).await?;
        outcome.rows_inserted += inserted;
        outcome.duplicate_rows += duplicates;
        conflicting.extend(conflicts);

        let inserted = insert_alerts(tx, &plan).await?;
        outcome.rows_inserted += inserted;
        outcome.duplicate_rows += count(plan.alerts.len()).saturating_sub(inserted);

        let inserted = insert_errors(tx, &plan).await?;
        outcome.rows_inserted += inserted;
        outcome.duplicate_rows += count(plan.errors.len()).saturating_sub(inserted);

        outcome.conflicting_records = count(conflicting.len());
        let unprocessed = plan.unprocessed_rows(&conflicting);
        outcome.unprocessed_inserted = insert_unprocessed(tx, records, &unprocessed).await?;
        Ok((outcome, new_vehicles))
    }

    /// Stores one record that the database rejected: `database_rejected`
    /// with the SQLSTATE, and its decode reason, if any.
    ///
    /// When the database also rejects the decode reason (a new reason
    /// without its migration), the record is stored with
    /// `database_rejected` alone. That row holds only bytes, a reason of
    /// the CHECK list, and a SQLSTATE, so the schema accepts it.
    async fn reject(
        &self,
        record: &QueuedRecord<'_>,
        state: SqlState,
        progress: &mut Progress<'_>,
    ) -> Result<WriteOutcome, StorageError> {
        let rejected = UnprocessedRow {
            record: 0,
            reason: Reason::DatabaseRejected,
            sqlstate: Some(state),
        };
        let mut rows = vec![rejected];
        if let Some(reason) = record.decoded.unprocessed_reason() {
            rows.push(UnprocessedRow {
                record: 0,
                reason: Reason::Decode(reason),
                sqlstate: None,
            });
        }
        let mut inserted = None;
        for rows in [&rows[..], &rows[..1]] {
            progress.attempt();
            match self.insert_rejected(record, rows).await? {
                Ok(count) => {
                    inserted = Some(count);
                    break;
                }
                Err(state) if rows.len() == 1 => {
                    return Err(StorageError::new(
                        FailureClass::Data,
                        ErrorCode::Sqlstate(state),
                    ));
                }
                Err(_) => {}
            }
        }
        Ok(WriteOutcome {
            transactions: 1,
            rejected_records: 1,
            unprocessed_inserted: inserted.unwrap_or(0),
            ..WriteOutcome::default()
        })
    }

    /// Inserts the unprocessed rows of one rejected record in a transaction
    /// of its own. The inner `Err` is a data error of the insert.
    async fn insert_rejected(
        &self,
        record: &QueuedRecord<'_>,
        rows: &[UnprocessedRow],
    ) -> Result<Result<u64, SqlState>, StorageError> {
        within(self.call_deadline, self.insert_rejected_now(record, rows)).await
    }

    async fn insert_rejected_now(
        &self,
        record: &QueuedRecord<'_>,
        rows: &[UnprocessedRow],
    ) -> Result<Result<u64, SqlState>, StorageError> {
        let mut call = CallConnection::acquire(&self.pool).await?;
        let result = insert_rejected_on(call.get()?, record, rows).await;
        call.finish(
            result
                .as_ref()
                .map_or_else(|error| error.answered(), |_| true),
        );
        result
    }
}

async fn insert_rejected_on(
    conn: &mut PgConnection,
    record: &QueuedRecord<'_>,
    rows: &[UnprocessedRow],
) -> Result<Result<u64, SqlState>, StorageError> {
    let mut tx = conn.begin().await.map_err(|error| statement(&error))?;
    match insert_unprocessed(&mut tx, std::slice::from_ref(record), rows).await {
        Ok(inserted) => {
            tx.commit().await.map_err(|error| statement(&error))?;
            Ok(Ok(inserted))
        }
        Err(error) => {
            let _rolled_back = tx.rollback().await;
            match error {
                AttemptError::Data(state) => Ok(Err(state)),
                AttemptError::Other(error) => Err(error),
            }
        }
    }
}

/// Splits the records into ranges of at most
/// [`MAX_SIGNAL_ROWS_PER_TRANSACTION`] signal rows and
/// [`MAX_BYTES_PER_TRANSACTION`] bytes of messages. A record above a bound
/// is a range of its own.
fn chunks(records: &[QueuedRecord<'_>]) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut signals: usize = 0;
    let mut bytes: usize = 0;
    for (index, record) in records.iter().enumerate() {
        let rows = match record.decoded {
            crate::record::Decoded::VehicleData(data) => data.signals.len(),
            _ => 0,
        };
        let size = record.topic.len().saturating_add(record.payload.len());
        let full = signals + rows > MAX_SIGNAL_ROWS_PER_TRANSACTION
            || bytes.saturating_add(size) > MAX_BYTES_PER_TRANSACTION;
        if index > start && full {
            ranges.push(start..index);
            start = index;
            signals = 0;
            bytes = 0;
        }
        signals += rows;
        bytes = bytes.saturating_add(size);
    }
    if start < records.len() {
        ranges.push(start..records.len());
    }
    ranges
}

fn count(length: usize) -> u64 {
    u64::try_from(length).unwrap_or(u64::MAX)
}

/// Finds the ID of a VIN, or inserts the vehicle.
///
/// The select comes first: an `INSERT ... ON CONFLICT DO NOTHING` takes an
/// identity value also when it inserts nothing, and the ID is a `smallint`.
/// When another writer inserts the same VIN at the same time, the insert
/// returns no row and the second select finds the row of the other writer.
async fn vehicle_id(
    tx: &mut Transaction<'_, Postgres>,
    vin: &str,
    first_seen_at: jiff::Timestamp,
) -> Result<VehicleId, AttemptError> {
    let select = || sqlx::query_scalar!("SELECT id FROM vehicle WHERE vin = $1", vin);
    if let Some(id) = select()
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| statement(&error))?
    {
        return Ok(id);
    }
    let inserted = sqlx::query_scalar!(
        "INSERT INTO vehicle (vin, first_seen_at) VALUES ($1, $2)
         ON CONFLICT (vin) DO NOTHING
         RETURNING id",
        vin,
        first_seen_at.to_sqlx() as _,
    )
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| statement(&error))?;
    if let Some(id) = inserted {
        return Ok(id);
    }
    select()
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| statement(&error))?
        .ok_or(AttemptError::Other(StorageError::new(
            FailureClass::Answered,
            ErrorCode::Other,
        )))
}

/// Inserts the signal rows. Returns the new rows, the rows equal to a
/// stored row, and the records whose value differs from the stored row.
async fn insert_signals(
    tx: &mut Transaction<'_, Postgres>,
    plan: &Plan,
) -> Result<(u64, u64, BTreeSet<usize>), AttemptError> {
    if plan.signals.is_empty() {
        return Ok((0, 0, BTreeSet::new()));
    }
    let rows = &plan.signals;
    let created_at: Vec<_> = rows
        .iter()
        .map(|row| row.key.created_at.to_sqlx())
        .collect();
    let received_at: Vec<_> = rows.iter().map(|row| row.received_at.to_sqlx()).collect();
    let int_value: Vec<Option<i64>> = rows.iter().map(|row| row.value.int_value).collect();
    let float_value: Vec<Option<f64>> = rows
        .iter()
        .map(|row| row.value.float_bits.map(f64::from_bits))
        .collect();
    let latitude: Vec<Option<f64>> = rows
        .iter()
        .map(|row| row.value.latitude_bits.map(f64::from_bits))
        .collect();
    let longitude: Vec<Option<f64>> = rows
        .iter()
        .map(|row| row.value.longitude_bits.map(f64::from_bits))
        .collect();
    let field: Vec<i32> = rows.iter().map(|row| row.key.field).collect();
    let vehicle_id: Vec<i16> = rows.iter().map(|row| row.key.vehicle_id).collect();
    let value_type: Vec<i16> = rows.iter().map(|row| row.value.value_type).collect();
    let is_resend: Vec<bool> = rows.iter().map(|row| row.is_resend).collect();
    let bool_value: Vec<Option<bool>> = rows.iter().map(|row| row.value.bool_value).collect();
    let text_value: Vec<Option<&str>> = rows
        .iter()
        .map(|row| row.value.text_value.as_deref())
        .collect();
    let json_value: Vec<Option<&serde_json::Value>> = rows
        .iter()
        .map(|row| row.value.json_value.as_ref())
        .collect();

    let inserted = sqlx::query!(
        r#"INSERT INTO vehicle_signal (
               created_at, received_at, int_value, float_value, latitude, longitude,
               field, vehicle_id, value_type, is_resend, bool_value, text_value, json_value
           )
           SELECT * FROM UNNEST(
               $1::timestamptz[], $2::timestamptz[], $3::bigint[], $4::double precision[],
               $5::double precision[], $6::double precision[], $7::integer[], $8::smallint[],
               $9::smallint[], $10::boolean[], $11::boolean[], $12::text[], $13::jsonb[]
           )
           ON CONFLICT DO NOTHING
           RETURNING vehicle_id, field, created_at AS "created_at: jiff_sqlx::Timestamp""#,
        &created_at as _,
        &received_at as _,
        &int_value as _,
        &float_value as _,
        &latitude as _,
        &longitude as _,
        &field,
        &vehicle_id,
        &value_type,
        &is_resend,
        &bool_value as _,
        &text_value as _,
        &json_value as _,
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| statement(&error))?;

    let inserted: HashSet<SignalKey> = inserted
        .into_iter()
        .map(|row| SignalKey {
            vehicle_id: row.vehicle_id,
            field: row.field,
            created_at: row.created_at.to_jiff(),
        })
        .collect();
    let candidates: Vec<(SignalKey, &SignalValueColumns, usize)> = rows
        .iter()
        .filter(|row| !inserted.contains(&row.key))
        .map(|row| (row.key, &row.value, row.record))
        .collect();
    let new_rows = count(inserted.len());
    if candidates.is_empty() {
        return Ok((new_rows, 0, BTreeSet::new()));
    }
    let stored = stored_signals(tx, &candidates).await?;
    let (duplicates, conflicting) = compare_stored(&candidates, &stored);
    Ok((new_rows, duplicates, conflicting))
}

/// Reads the stored values of the given signal keys.
async fn stored_signals(
    tx: &mut Transaction<'_, Postgres>,
    candidates: &[(SignalKey, &SignalValueColumns, usize)],
) -> Result<HashMap<SignalKey, SignalValueColumns>, AttemptError> {
    let vehicle_id: Vec<i16> = candidates
        .iter()
        .map(|(key, _, _)| key.vehicle_id)
        .collect();
    let field: Vec<i32> = candidates.iter().map(|(key, _, _)| key.field).collect();
    let created_at: Vec<_> = candidates
        .iter()
        .map(|(key, _, _)| key.created_at.to_sqlx())
        .collect();
    let rows = sqlx::query!(
        r#"SELECT s.vehicle_id, s.field, s.created_at AS "created_at: jiff_sqlx::Timestamp",
                  s.value_type, s.int_value, s.float_value, s.latitude, s.longitude,
                  s.bool_value, s.text_value, s.json_value
           FROM UNNEST($1::smallint[], $2::integer[], $3::timestamptz[])
                AS k (vehicle_id, field, created_at)
           JOIN vehicle_signal AS s USING (vehicle_id, field, created_at)"#,
        &vehicle_id,
        &field,
        &created_at as _,
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| statement(&error))?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let key = SignalKey {
                vehicle_id: row.vehicle_id,
                field: row.field,
                created_at: row.created_at.to_jiff(),
            };
            let value = SignalValueColumns {
                value_type: row.value_type,
                int_value: row.int_value,
                float_bits: row.float_value.map(f64::to_bits),
                latitude_bits: row.latitude.map(f64::to_bits),
                longitude_bits: row.longitude.map(f64::to_bits),
                bool_value: row.bool_value,
                text_value: row.text_value,
                json_value: row.json_value,
            };
            (key, value)
        })
        .collect())
}

/// Inserts the connectivity rows. Returns the same counts as
/// [`insert_signals`].
async fn insert_connectivity(
    tx: &mut Transaction<'_, Postgres>,
    plan: &Plan,
) -> Result<(u64, u64, BTreeSet<usize>), AttemptError> {
    if plan.connectivity.is_empty() {
        return Ok((0, 0, BTreeSet::new()));
    }
    let rows = &plan.connectivity;
    let connection_id: Vec<uuid::Uuid> = rows.iter().map(|row| row.key.connection_id).collect();
    let created_at: Vec<_> = rows
        .iter()
        .map(|row| row.key.created_at.to_sqlx())
        .collect();
    let received_at: Vec<_> = rows.iter().map(|row| row.received_at.to_sqlx()).collect();
    let status: Vec<i32> = rows.iter().map(|row| row.key.status).collect();
    let vehicle_id: Vec<i16> = rows.iter().map(|row| row.value.vehicle_id).collect();
    let network_interface: Vec<&str> = rows
        .iter()
        .map(|row| row.value.network_interface.as_str())
        .collect();

    let inserted = sqlx::query!(
        r#"INSERT INTO connectivity_event (
               connection_id, created_at, received_at, status, vehicle_id, network_interface
           )
           SELECT * FROM UNNEST(
               $1::uuid[], $2::timestamptz[], $3::timestamptz[], $4::integer[],
               $5::smallint[], $6::text[]
           )
           ON CONFLICT DO NOTHING
           RETURNING connection_id, status, created_at AS "created_at: jiff_sqlx::Timestamp""#,
        &connection_id,
        &created_at as _,
        &received_at as _,
        &status,
        &vehicle_id,
        &network_interface as _,
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| statement(&error))?;

    let inserted: HashSet<ConnectivityKey> = inserted
        .into_iter()
        .map(|row| ConnectivityKey {
            connection_id: row.connection_id,
            status: row.status,
            created_at: row.created_at.to_jiff(),
        })
        .collect();
    let candidates: Vec<(ConnectivityKey, &ConnectivityValue, usize)> = rows
        .iter()
        .filter(|row| !inserted.contains(&row.key))
        .map(|row| (row.key, &row.value, row.record))
        .collect();
    let new_rows = count(inserted.len());
    if candidates.is_empty() {
        return Ok((new_rows, 0, BTreeSet::new()));
    }
    let connection_id: Vec<uuid::Uuid> = candidates
        .iter()
        .map(|(key, _, _)| key.connection_id)
        .collect();
    let status: Vec<i32> = candidates.iter().map(|(key, _, _)| key.status).collect();
    let created_at: Vec<_> = candidates
        .iter()
        .map(|(key, _, _)| key.created_at.to_sqlx())
        .collect();
    let stored = sqlx::query!(
        r#"SELECT c.connection_id, c.status, c.created_at AS "created_at: jiff_sqlx::Timestamp",
                  c.vehicle_id, c.network_interface
           FROM UNNEST($1::uuid[], $2::integer[], $3::timestamptz[])
                AS k (connection_id, status, created_at)
           JOIN connectivity_event AS c USING (connection_id, status, created_at)"#,
        &connection_id,
        &status,
        &created_at as _,
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| statement(&error))?;
    let stored: HashMap<ConnectivityKey, ConnectivityValue> = stored
        .into_iter()
        .map(|row| {
            (
                ConnectivityKey {
                    connection_id: row.connection_id,
                    status: row.status,
                    created_at: row.created_at.to_jiff(),
                },
                ConnectivityValue {
                    vehicle_id: row.vehicle_id,
                    network_interface: row.network_interface,
                },
            )
        })
        .collect();
    let (duplicates, conflicting) = compare_stored(&candidates, &stored);
    Ok((new_rows, duplicates, conflicting))
}

/// Inserts the alert rows and returns the number of new rows. A replay of a
/// stored alert state inserts nothing.
async fn insert_alerts(
    tx: &mut Transaction<'_, Postgres>,
    plan: &Plan,
) -> Result<u64, AttemptError> {
    if plan.alerts.is_empty() {
        return Ok(0);
    }
    let rows = &plan.alerts;
    let vehicle_id: Vec<i16> = rows.iter().map(|row| row.vehicle_id).collect();
    let name: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
    let audiences: Vec<&str> = rows.iter().map(|row| row.audiences.as_str()).collect();
    let started_at: Vec<_> = rows
        .iter()
        .map(|row| row.started_at.map(ToSqlx::to_sqlx))
        .collect();
    let ended_at: Vec<_> = rows
        .iter()
        .map(|row| row.ended_at.map(ToSqlx::to_sqlx))
        .collect();
    let created_at: Vec<_> = rows.iter().map(|row| row.created_at.to_sqlx()).collect();
    let received_at: Vec<_> = rows.iter().map(|row| row.received_at.to_sqlx()).collect();
    let result = sqlx::query!(
        r#"INSERT INTO vehicle_alert (
               vehicle_id, name, audiences, started_at, ended_at, created_at, received_at
           )
           SELECT vehicle_id, name, audiences::integer[], started_at, ended_at, created_at, received_at
           FROM UNNEST(
               $1::smallint[], $2::text[], $3::text[], $4::timestamptz[], $5::timestamptz[],
               $6::timestamptz[], $7::timestamptz[]
           ) AS r (vehicle_id, name, audiences, started_at, ended_at, created_at, received_at)
           ON CONFLICT DO NOTHING"#,
        &vehicle_id,
        &name as _,
        &audiences as _,
        &started_at as _,
        &ended_at as _,
        &created_at as _,
        &received_at as _,
    )
    .execute(&mut **tx)
    .await
    .map_err(|error| statement(&error))?;
    Ok(result.rows_affected())
}

/// Inserts the error rows and returns the number of new rows.
async fn insert_errors(
    tx: &mut Transaction<'_, Postgres>,
    plan: &Plan,
) -> Result<u64, AttemptError> {
    if plan.errors.is_empty() {
        return Ok(0);
    }
    let rows = &plan.errors;
    let vehicle_id: Vec<i16> = rows.iter().map(|row| row.vehicle_id).collect();
    let created_at: Vec<_> = rows
        .iter()
        .map(|row| row.created_at.map(ToSqlx::to_sqlx))
        .collect();
    let name: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
    let tags: Vec<&serde_json::Value> = rows.iter().map(|row| &row.tags).collect();
    let body: Vec<&str> = rows.iter().map(|row| row.body.as_str()).collect();
    let record_created_at: Vec<_> = rows
        .iter()
        .map(|row| row.record_created_at.to_sqlx())
        .collect();
    let received_at: Vec<_> = rows.iter().map(|row| row.received_at.to_sqlx()).collect();
    let result = sqlx::query!(
        r#"INSERT INTO vehicle_error (
               vehicle_id, created_at, name, tags, body, record_created_at, received_at
           )
           SELECT * FROM UNNEST(
               $1::smallint[], $2::timestamptz[], $3::text[], $4::jsonb[], $5::text[],
               $6::timestamptz[], $7::timestamptz[]
           )
           ON CONFLICT DO NOTHING"#,
        &vehicle_id,
        &created_at as _,
        &name as _,
        &tags as _,
        &body as _,
        &record_created_at as _,
        &received_at as _,
    )
    .execute(&mut **tx)
    .await
    .map_err(|error| statement(&error))?;
    Ok(result.rows_affected())
}

/// Inserts the unprocessed rows and returns the number of new rows. A row
/// with the same topic, reason, and payload as a stored row inserts
/// nothing.
async fn insert_unprocessed(
    tx: &mut Transaction<'_, Postgres>,
    records: &[QueuedRecord<'_>],
    rows: &[UnprocessedRow],
) -> Result<u64, AttemptError> {
    if rows.is_empty() {
        return Ok(0);
    }
    let mut received_at = Vec::with_capacity(rows.len());
    let mut topic = Vec::with_capacity(rows.len());
    let mut payload = Vec::with_capacity(rows.len());
    let mut reason = Vec::with_capacity(rows.len());
    let mut sqlstate = Vec::with_capacity(rows.len());
    for row in rows {
        let record = records
            .get(row.record)
            .ok_or(AttemptError::Other(StorageError::new(
                FailureClass::Answered,
                ErrorCode::Other,
            )))?;
        received_at.push(floor_micros(record.received_at()).to_sqlx());
        topic.push(record.topic);
        payload.push(record.payload);
        reason.push(row.reason.as_str());
        sqlstate.push(row.sqlstate.map(|state| state.to_string()));
    }
    let result = sqlx::query!(
        r#"INSERT INTO unprocessed_record (received_at, topic, payload, reason, sqlstate)
           SELECT * FROM UNNEST(
               $1::timestamptz[], $2::bytea[], $3::bytea[], $4::text[], $5::text[]
           )
           ON CONFLICT DO NOTHING"#,
        &received_at as _,
        &topic as _,
        &payload as _,
        &reason as _,
        &sqlstate as _,
    )
    .execute(&mut **tx)
    .await
    .map_err(|error| statement(&error))?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::record::{
        Decoded, Signal, SignalValue, Unprocessed, UnprocessedReason, VehicleData, Vin,
    };

    fn vehicle_data(signals: usize) -> Decoded {
        Decoded::VehicleData(VehicleData {
            vin: Vin::new("TESTVIN0000000001".to_owned()),
            created_at: Timestamp::UNIX_EPOCH,
            received_at: Timestamp::UNIX_EPOCH,
            is_resend: false,
            signals: (0..signals)
                .map(|index| Signal {
                    field: i32::try_from(index).unwrap(),
                    value: SignalValue::Int32(1),
                })
                .collect(),
            unknown_value_variants: 0,
            unknown_fields: false,
        })
    }

    fn sizes(records: &[QueuedRecord<'_>]) -> Vec<usize> {
        chunks(records).iter().map(ExactSizeIterator::len).collect()
    }

    #[test]
    fn chunks_hold_at_most_5000_signal_rows_and_never_split_a_record() {
        let big = vehicle_data(3_000);
        let small = vehicle_data(10);
        let huge = vehicle_data(6_000);
        let none = Decoded::Unprocessed(Unprocessed {
            reason: UnprocessedReason::UnknownTopic,
            received_at: Timestamp::UNIX_EPOCH,
        });
        let record = |decoded| QueuedRecord {
            topic: b"t",
            payload: b"p",
            decoded,
        };
        assert_eq!(sizes(&[]), Vec::<usize>::new());
        assert_eq!(sizes(&[record(&small), record(&none)]), vec![2]);
        // 3000 + 3000 > 5000: two transactions.
        assert_eq!(sizes(&[record(&big), record(&big)]), vec![1, 1]);
        // 3000 + 10 + 10 fits; the next 3000 does not.
        assert_eq!(
            sizes(&[record(&big), record(&small), record(&small), record(&big)]),
            vec![3, 1]
        );
        // A record above the limit is a transaction of its own.
        assert_eq!(
            sizes(&[record(&small), record(&huge), record(&small)]),
            vec![1, 1, 1]
        );
        // Records without signal rows do not count.
        assert_eq!(
            sizes(&[record(&big), record(&none), record(&none)]),
            vec![3]
        );
    }

    #[test]
    fn the_signal_bound_is_exactly_5000_rows() {
        let half = vehicle_data(2_500);
        let more = vehicle_data(2_501);
        let record = |decoded| QueuedRecord {
            topic: b"t",
            payload: b"p",
            decoded,
        };
        assert_eq!(MAX_SIGNAL_ROWS_PER_TRANSACTION, 5_000);
        assert_eq!(sizes(&[record(&half), record(&half)]), vec![2]);
        assert_eq!(sizes(&[record(&half), record(&more)]), vec![1, 1]);
    }

    #[test]
    fn the_byte_bound_splits_large_records_without_signals() {
        let none = Decoded::Unprocessed(Unprocessed {
            reason: UnprocessedReason::DecodeFailed,
            received_at: Timestamp::UNIX_EPOCH,
        });
        let quarter = vec![0_u8; MAX_BYTES_PER_TRANSACTION / 4 - 1];
        let large = vec![0_u8; MAX_BYTES_PER_TRANSACTION + 1];
        let record = |payload| QueuedRecord {
            topic: b"t",
            payload,
            decoded: &none,
        };
        // Four records of a quarter, each 1 byte of topic: exactly the bound.
        let four: Vec<_> = (0..4).map(|_| record(&quarter)).collect();
        assert_eq!(sizes(&four), vec![4]);
        let five: Vec<_> = (0..5).map(|_| record(&quarter)).collect();
        assert_eq!(sizes(&five), vec![4, 1]);
        // A record above the bound is a transaction of its own.
        assert_eq!(
            sizes(&[record(&quarter), record(&large), record(&quarter)]),
            vec![1, 1, 1]
        );
    }

    #[test]
    fn chunks_cover_every_record_in_order() {
        let small = vehicle_data(4_000);
        let record = |decoded| QueuedRecord {
            topic: b"t",
            payload: b"p",
            decoded,
        };
        let records: Vec<_> = (0..5).map(|_| record(&small)).collect();
        assert_eq!(chunks(&records), vec![0..1, 1..2, 2..3, 3..4, 4..5]);
    }
}
