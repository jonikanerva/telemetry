//! PostgreSQL storage of the raw records (ADR 0002).
//!
//! # Interface for the pipeline
//!
//! The pipeline uses these items: the queue, the flush timer, and the
//! retry loop of ADR 0001. The storage has no queue, no timer, and no
//! retry of its own.
//!
//! | Step | Call |
//! | ---- | ---- |
//! | Connection settings of `ingest run` | [`connect_options`] with `DATABASE_URL`, [`Schema::raw`], and [`RUN_STATEMENT_TIMEOUT`] |
//! | Pool | [`run_pool`]: at most 2 connections, none while idle |
//! | Start-up check (gate condition 3) | [`check_migrations`] |
//! | Flush | [`Writer::write`] with the queued records in queue order; [`WriteOutcome`] has the counts, [`WriteError`] the failure |
//! | Events | [`write_events`] with the run ID and the events of the event sink ([`crate::events::IngestEvent`]) |
//! | Failure | [`StorageError`]: [`FailureClass`] and a static [`ErrorCode`] |
//!
//! A queued record is a [`QueuedRecord`]: the two ZMQ frames and their
//! [`Decoded`](crate::record::Decoded) value.
//!
//! # How the caller treats a failure
//!
//! | Call | Class | What is stored | What the caller does |
//! | ---- | ----- | -------------- | -------------------- |
//! | [`Writer::write`] | `Ok` | Every record: rows, or `unprocessed_record` rows | Remove the records from the queue. |
//! | [`Writer::write`] | [`FailureClass::NoAnswer`] | `records[..committed]` ([`WriteError::committed`]) | Keep every record. Retry after 1 s, doubling to at most 5 minutes (ADR 0001). The records before `committed` become duplicates, or the caller removes them first. |
//! | [`Writer::write`] | [`FailureClass::Answered`] | `records[..committed]` | Keep every record. Retry as above; after three equal codes in a row, retry once each 60 minutes (ADR 0001). |
//! | [`Writer::write`] | [`FailureClass::Data`] | `records[..committed]` | A defect: the database refused even the `database_rejected` row of the record `records[committed]`, so its bytes cannot be stored. Log the code and the count, remove `records[..=committed]` from the queue, and write the rest. A retry would fail the same way without end. |
//! | [`write_events`] | `Ok` | Every event, except the counted `rejected` ones | Remove the events; log a count of rejected events, which cannot be stored. |
//! | [`write_events`] | `NoAnswer` or `Answered` | Nothing of this call | Keep the events and retry with the cadence above. |
//! | [`check_migrations`] | `NoAnswer` | | Retry; degraded mode after 120 s (ADR 0001, gate condition 3). |
//! | [`check_migrations`] | `Answered` | | Keep the gate closed; retry with the cadence above. |
//!
//! The classes in detail (`error.rs`):
//!
//! - `NoAnswer`: no session started (network, TLS, the connect timeout of
//!   15 s, a server error before the session other than class 28), the
//!   connection broke in a session (I/O, protocol, the pool closed), or a
//!   call passed its client deadline. A certificate failure comes back as
//!   code `io`.
//! - `Answered`: an authentication error (class 28), a missing or changed
//!   migration, a missing schema, and every SQLSTATE in a session outside
//!   classes 22 and 23. This includes `57014` (statement timeout), `53100`
//!   (disk full), `40P01` (deadlock), and `57P01` (the server ended the
//!   session, for example at a compute restart; the transaction rolls back
//!   and a retry stores every row once, tested by the reviewer of PR #19).
//!   So three `57P01` in a row move the retry to the 60-minute cadence; the
//!   pipeline may treat class `57P` and `08` as no answer for its cadence.
//! - `Data`: classes 22 and 23. `write` handles them by the split and
//!   returns them only in the defect case of the table.
//!
//! # Deadlines
//!
//! | Path | Bound |
//! | ---- | ----- |
//! | Connect of the pool, also inside a write | 15 s ([`CONNECT_TIMEOUT`], acquire timeout) |
//! | Connect of [`connect`] (`ingest migrate`, the role script) | 15 s on the client |
//! | One statement of `ingest run`, also a `COMMIT` and the lock wait of a competing writer | 30 s on the server ([`RUN_STATEMENT_TIMEOUT`]) |
//! | One transaction of [`Writer::write`], one insert of [`write_events`], [`check_migrations`] | 90 s on the client ([`RUN_CALL_DEADLINE`]), also for a connection that goes silent in a session |
//! | One statement of `ingest migrate`, also the wait for the advisory lock of the migrator | 5 minutes on the server ([`MIGRATE_STATEMENT_TIMEOUT`]) |
//! | All of [`migrate`] | 15 minutes on the client ([`MIGRATE_DEADLINE`]) |
//! | [`apply_roles`], also the wait for the advisory lock of the script | the statement timeout of its connection on the server, 90 s on the client |
//!
//! A [`Writer::write`] call with several transactions has no total bound;
//! each transaction has its own, and the pipeline beats the watchdog
//! between them.

//! # Decisions
//!
//! These points complete ADR 0002. Each has a test.
//!
//! - **Wire numbers and column types.** `field`, `status`, and
//!   `audiences` are `int32` enums on the wire, and the vehicle can send
//!   any number, also one that the vendored schema does not know. Their
//!   columns are `integer` and `integer[]`, so every wire value fits and
//!   no value needs a range check. The largest `Field` number of the
//!   vendored schema is 280; `integer` costs 8 bytes more per signal row
//!   than `smallint` after alignment (measured, `STACK.md` §5). The
//!   numbers that the process makes itself stay `smallint`: `value_type`
//!   (at most 60 in the schema) and the vehicle ID.
//! - **NaN and the duplicate rule.** A copy of a key is a duplicate only
//!   when the variant and every column are equal. Floats compare by their
//!   bits, in the batch and against the table, so a NaN equals the same
//!   NaN, and `0.0` and `-0.0` differ. PostgreSQL returns the bits that it
//!   stored. Any other difference makes the record a
//!   `conflicting_duplicate`, so no value is lost silently.
//! - **VIN.** The VIN is stored as sent when it has 1 to 64 bytes. An empty
//!   VIN, a longer one, or one with a NUL byte fails a constraint (SQLSTATE
//!   class 22 or 23); the split isolates the record, and it goes to
//!   `unprocessed_record` with reason `database_rejected`. A VIN of another
//!   form than 17 characters is stored as sent: it is the identity that
//!   `fleet-telemetry` gives the connection.
//! - **Topic bytes.** `unprocessed_record.topic` is `bytea`, not `text`. A
//!   topic can be any bytes, also bytes that are not UTF-8 or a NUL byte,
//!   which `text` cannot hold. The key uses `md5(topic)`.
//! - **Text that PostgreSQL rejects.** A NUL byte in a text value or in a
//!   JSON string fails the insert (SQLSTATE 22021 or 22P05). The writer
//!   does not check text before the insert: the split isolates the record,
//!   which keeps the real SQLSTATE. Keys of unknown length use `md5()`, so
//!   no value can fail with SQLSTATE 54000 (index row too large), which is
//!   not a data error and would be retried without end.
//! - **Time precision.** Every instant is truncated to whole microseconds
//!   toward the past before the bind: the times of the payload (already
//!   truncated by the decode), the receive time, and the event times.
//! - **New reasons.** The CHECK constraint on `unprocessed_record.reason`
//!   stays. A change that adds a reason must add it to the constraint in a
//!   migration of the same release. The release order is then safe by
//!   construction: the pre-deploy command applies the migration before
//!   the new binary runs, and `ingest run` keeps the gate closed until
//!   every migration that it embeds is applied ([`check_migrations`]).
//!   If a reason still fails the constraint, the record goes to
//!   `unprocessed_record` with reason `database_rejected` and SQLSTATE
//!   23514, so the original bytes are kept.
//!
//! # Schema name in the tests
//!
//! The SQL names no schema. [`connect_options`] sets the `search_path` to
//! the schema only. Production passes [`Schema::raw`]; the tests pass a
//! schema of their own for each test, so test runs on the shared test
//! branch do not see each other's tables.

mod connect;
mod error;
mod events;
mod migrate;
mod roles;
mod rows;
mod writer;

pub use connect::{
    CONNECT_TIMEOUT, MIGRATE_DEADLINE, MIGRATE_STATEMENT_TIMEOUT, RUN_CALL_DEADLINE,
    RUN_STATEMENT_TIMEOUT, Schema, connect, connect_options, run_pool,
};
pub use error::{ErrorCode, FailureClass, SqlState, StorageError};
pub use events::{EventOutcome, write_events};
pub use migrate::{MIGRATOR, MigrateOutcome, check_migrations, migrate};
pub use roles::{INGEST_ROLE, MIGRATOR_ROLE, ROLES_SQL, RoleSetup, apply_roles};
pub use rows::{QueuedRecord, UNPROCESSED_REASONS};
pub use writer::{
    MAX_BYTES_PER_TRANSACTION, MAX_SIGNAL_ROWS_PER_TRANSACTION, WriteError, WriteOutcome, Writer,
};
