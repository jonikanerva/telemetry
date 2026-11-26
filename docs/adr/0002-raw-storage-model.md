# ADR 0002: Raw storage model

- Status: Accepted, 26 November 2026. Issue #4. The lead decided the open
  points within delegated authority on 26 November 2026 (ADR 0001, section
  "Lead decisions"). Amended by the storage change (issue #12): column
  types of the wire numbers, keys of unknown length, the topic as bytes,
  data errors through the batch split, the privileges through default
  privileges, and the measured size.
- Scope: the PostgreSQL schema for raw records, the database roles, and the
  migration step. ADR 0001 owns the path to the database.

## Context

- The ingest process receives four record types as protobuf messages from
  `fleet-telemetry` v0.10.1 (ADR 0001): `V` (`Payload`), `alerts`
  (`VehicleAlerts`), `errors` (`VehicleErrors`), and `connectivity`
  (`VehicleConnectivity`) [P].
- A `Payload` has one `created_at` (vehicle clock), the VIN, `is_resend`,
  and a list of `Datum` items. Each `Datum` has a `Field` enum and a `Value`.
  `Value` is a `oneof` with about 55 variants: `string`, `int32`, `int64`,
  `float`, `double`, `bool`, `LocationValue`, the `invalid` marker, many
  enums, and three small messages (`Doors`, `Time`, `TireLocation`)
  [P, lines 752-834]. Tesla notes that a field can change its type between
  software versions.
- The ZMQ message carries no transaction ID and no receive time (ADR 0001,
  fact 4). A resent or duplicate record has the same content. Alerts were
  replayed on each new connection. The tuned vehicle config on `main` has
  no `alert_types`, so the alerts table can stay empty until a later
  config asks for alerts. The vehicle can hold two connections at
  the same time, so the same record can arrive twice.
- Units are not on the wire. Tesla documents a unit for some fields only,
  for example a speed change of "1mph" [T].
- `connectivity` events come from the server clock. A `DISCONNECTED` event
  is missing when `fleet-telemetry` dies
  ([server.go#L149-L160](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/server/streaming/server.go#L149-L160)).
- `prost` drops unknown fields. A newer `Value` variant decodes as `None`
  (ADR 0001). Upstream keeps unknown fields when it encodes a record again
  (Go `proto.Unmarshal` and `proto.Marshal` without options,
  [`telemetry/record.go#L152-L201`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/telemetry/record.go#L152-L201)).
- Neon Free plan: 1 GB storage for each project. Child branches count with
  their changes. Writes fail when the storage is full. 10 branches for each
  project [N1]. A role created with SQL is not a member of
  `neon_superuser`, and its password needs 60 bits of entropy [N2].
- `ON CONFLICT` needs `SELECT` on the columns that the arbiter reads. Without
  a conflict target, `DO NOTHING` handles every unique constraint [PG1].
  `NULLS NOT DISTINCT` makes NULL values equal in a unique constraint [PG2].
- Owner rule for this phase: keep the raw records. No deletion job.

## Decision

### Schema and tables

All tables are in schema `raw`. Each table stores the values as the vehicle
sent them, after the upstream transformations named in ADR 0001.

| Table                     | One row for                            | Idempotency key (unique)                                                       |
| ------------------------- | -------------------------------------- | ------------------------------------------------------------------------------ |
| `raw.vehicle`             | each VIN                               | `vin`                                                                          |
| `raw.vehicle_signal`      | each `Datum` of a `V` record           | primary key `(vehicle_id, field, created_at)`                                  |
| `raw.vehicle_alert`       | each distinct alert state              | unique index `(vehicle_id, md5(name), started_at, ended_at, audiences)` `NULLS NOT DISTINCT` |
| `raw.vehicle_error`       | each error item                        | unique index `(vehicle_id, created_at, md5(name), md5(tags::text), md5(body))` `NULLS NOT DISTINCT` |
| `raw.connectivity_event`  | each connect or disconnect             | primary key `(connection_id, status, created_at)`                              |
| `raw.unprocessed_record`  | each record that cannot be stored as rows | unique index `(md5(topic), reason, md5(payload))`                           |
| `raw.ingest_event`        | each event of the ingest process       | `(run_id, kind, occurred_at)`                                                  |

Columns:

- `raw.vehicle`: `id smallint` identity, `vin text NOT NULL UNIQUE` with 1
  to 64 bytes (CHECK; 64 is the X.509 limit of the certificate common name
  that `fleet-telemetry` reads the VIN from), `first_seen_at timestamptz
  NOT NULL`. The VIN is stored only here, as sent.
- `raw.vehicle_signal`, in this order for alignment:
  `created_at timestamptz NOT NULL` (`Payload.created_at`),
  `received_at timestamptz NOT NULL`, `int_value bigint`,
  `float_value double precision`, `latitude double precision`,
  `longitude double precision`, `field integer NOT NULL` (number of the
  `Field` enum), `vehicle_id smallint NOT NULL` (foreign key),
  `value_type smallint NOT NULL` (field number of the `Value` variant),
  `is_resend boolean NOT NULL`, `bool_value boolean`, `text_value text`,
  `json_value jsonb`.
- Wire numbers: `field`, the alert `audiences`, and the connectivity
  `status` are `int32` enums on the wire, and the vehicle can send any
  number. Their columns are `integer`, so every value fits without a range
  check. The largest `Field` number of the vendored schema is 280; the
  wider column costs 8 B for each signal row (measured, "Expected size").
  The process makes `value_type` (at most 60) and the vehicle ID itself;
  they stay `smallint`.
- `raw.vehicle_alert`: `vehicle_id`, `name text NOT NULL`,
  `audiences integer[] NOT NULL` (enum numbers, at most 64, CHECK),
  `started_at timestamptz`, `ended_at timestamptz`, `created_at
  timestamptz NOT NULL` (of `VehicleAlerts`), `received_at timestamptz NOT
  NULL`.
- `raw.vehicle_error`: `vehicle_id`, `created_at timestamptz` (of the item),
  `name text NOT NULL`, `tags jsonb NOT NULL`, `body text NOT NULL`,
  `record_created_at timestamptz NOT NULL` (of `VehicleErrors`),
  `received_at timestamptz NOT NULL`.
- The record time is `NOT NULL` in every table, because a record without
  `created_at` goes to `raw.unprocessed_record` as a whole, for all four
  record types ("Unknown data"). The times of an alert or an error item
  (`started_at`, `ended_at`, `raw.vehicle_error.created_at`) can be
  missing and allow NULL.
- `raw.connectivity_event`: `vehicle_id`, `connection_id uuid NOT NULL`,
  `status integer NOT NULL` (enum number), `network_interface text NOT NULL`
  (as sent, also when empty), `created_at timestamptz NOT NULL` (server
  clock), `received_at timestamptz NOT NULL`.
- `raw.unprocessed_record`: `id bigint` identity, `topic bytea NOT NULL`
  (the first ZMQ frame; a topic can be any bytes, also bytes that are not
  UTF-8 or a NUL byte, which `text` cannot hold), `payload bytea NOT NULL`
  (the original protobuf bytes), `reason text NOT NULL`, `sqlstate text`,
  `received_at timestamptz NOT NULL`.
  A CHECK constraint limits `reason` to `decode_failed`, `unknown_topic`,
  `missing_created_at`, `unknown_value_variant`, `unknown_fields`,
  `conflicting_duplicate`, and `database_rejected`. `sqlstate` is set for
  `database_rejected` only, and it is five characters from `0`-`9` and
  `A`-`Z` (CHECK).
  A new reason needs a migration that adds it to the CHECK constraint, in
  the same release as the code. The order is then safe by construction:
  the pre-deploy command applies the migration before the new binary
  runs, and `ingest run` keeps the gate closed until every migration that
  it embeds is applied. A reason that still fails the constraint makes
  the record `database_rejected` with SQLSTATE 23514, so its bytes stay.
- `raw.ingest_event`: `id bigint` identity, `run_id uuid NOT NULL` (random
  for each process start), `occurred_at timestamptz NOT NULL`,
  `kind text NOT NULL` (`started`, `stopped`, `gate_closed`, `gate_opened`,
  `degraded_mode`, `child_exited`, `records_dropped`, `zmq_shortfall`,
  `gate_full`),
  `reason text`, `dropped_count bigint` (it holds every unsigned 32-bit
  count of the event sink),
  `first_received_at timestamptz`, `last_received_at timestamptz`. A
  `records_dropped`, `zmq_shortfall`, or `gate_full` row must have the
  count (at least 1) and both times, in order; the other kinds have none
  of them (CHECK). For `zmq_shortfall` and `gate_full`, the
  times are the window of the count (`gate_full` counts refused
  connections, at most one row each minute).

Indexes: the keys above, and one BRIN index on
`raw.vehicle_signal (created_at)`. The derived phase adds the indexes it
needs.

A key with text of unknown length uses `md5()` of the text. A btree entry
has a size limit, and a larger key fails with SQLSTATE 54000. That is not
a data error, so the writer would retry it without end and block the
queue. With `md5()` and the length limits above (VIN, audiences), only data
errors (classes 22 and 23) depend on the values. `md5()` is a compression of
the key, not a security measure; a collision would make two different
values one key.

### Value representation

`value_type` keeps the variant, so `int32` and `int64`, and `float` and
`double`, stay apart. One value column holds the value. A CHECK constraint
ties the column to the variant:

| `value_type` (proto field number)          | Column                     | Note                                               |
| ------------------------------------------ | -------------------------- | -------------------------------------------------- |
| 1 `string_value`                           | `text_value`               | Not parsed. `"12.3"` stays text.                   |
| 2 `int_value`, 3 `long_value`              | `int_value`                |                                                    |
| 4 `float_value`, 5 `double_value`          | `float_value`              | `float` widens to `double` without loss.           |
| 6 `boolean_value`, 10 `invalid`            | `bool_value`               | `invalid` is the "no valid value" marker. It is not a value of zero or false. |
| 7 `location_value`                         | `latitude`, `longitude`    | Both or none.                                      |
| 21 `door_value`, 31 `time_value`, 39 `tire_location_value` | `json_value` | Object with every proto field name, also `false` and 0. |
| every other number (an enum variant)       | `int_value`                | The enum number. Unknown numbers stay numbers.     |

No unit column. The vehicle does not send units. The stored number is in
the unit that the vehicle used. The derived phase owns a unit table per field
from Tesla's documentation, and marks a unit as unverified where Tesla
states none.

Unknown data:

- An unknown `Field` number is stored as its number.
- A `Datum` without a known `Value` variant gets no signal row. The complete
  record goes to `raw.unprocessed_record` with reason
  `unknown_value_variant`. The other `Datum` items of the record are stored.
  This is partial success within one record. The unprocessed row makes the
  exclusion visible.
- A record that does not decode, has an unknown topic, or has no
  `created_at` goes to `raw.unprocessed_record` as a whole. The
  `created_at` rule applies to all four record types.
- A record with a field that the vendored schema does not know, at any
  depth, keeps its rows, and the complete record also goes to
  `raw.unprocessed_record` with reason `unknown_fields`. `prost` skips
  such a field, but upstream keeps it when it encodes the record again, so
  a newer vehicle schema can send it. The decode compares each field
  number with the vendored schema to find it. A record with an unknown
  value variant gets only the reason `unknown_value_variant`.
- Text that PostgreSQL rejects (a NUL byte in `text` or in a JSON string),
  an empty or too long VIN, and too many audiences fail the insert with a
  data error (SQLSTATE 22021, 22P05, or 23514). The writer does not check
  values before the insert: the batch split isolates the record ("Write
  batching"), and the record goes to `raw.unprocessed_record` with reason
  `database_rejected` and the real SQLSTATE. None of its rows are stored;
  its bytes are. A VIN of 1 to 64 bytes in another form than 17
  characters is stored as sent: it is the identity that `fleet-telemetry`
  gives the connection.

### Time columns

- `created_at` is the instant from the record (`google.protobuf.Timestamp`).
  It is the time of the event. PostgreSQL keeps microseconds, so the ingest
  process truncates nanoseconds to microseconds.
- `received_at` is the clock of the ingest process when it reads the ZMQ
  message. It is not the receive time of `fleet-telemetry`, which ZMQ does
  not carry. For resent data, it can be much later than `created_at`.
- A record without `created_at` is never given an invented time.
- The storage truncates every instant to whole microseconds toward the
  past before it binds it: the times of the payload (the decode already
  did), the receive time, and the event times. A retry then binds the
  same value.

### Idempotency

Every insert uses `ON CONFLICT DO NOTHING`. A retry after a lost commit
acknowledgement is safe.

- Signals: the key excludes `is_resend` and `received_at`, because these
  differ between copies of the same datum. The first stored copy wins.
- Equal values: two copies of a signal key are equal only when the variant
  (`value_type`) and every value column are equal. Floats compare by their
  bits, in the batch and against the table: a NaN equals the same NaN,
  and `0.0` and `-0.0` differ. PostgreSQL returns the bits that it stored
  (tested). Every other difference is a conflict, so the record is kept.
  The same rule applies to the connectivity key, with the vehicle and the
  network interface as the values.
- Same key inside one batch: `RETURNING` cannot see a conflict between two
  rows of the same statement. The writer therefore compares the keys of a
  batch in memory first. An equal value is a duplicate and is dropped from
  the batch with a count. A different value sends the later record to
  `raw.unprocessed_record` with reason `conflicting_duplicate`.
- Same key as a stored row: the writer uses `RETURNING` to find the rows
  that were not inserted. Only then does it read the stored rows for these
  keys. An equal value is a duplicate and is only counted. A different
  value sends the newer record to `raw.unprocessed_record` with reason
  `conflicting_duplicate`. No value is lost silently.
- Alerts: an active alert has `ended_at` NULL. `NULLS NOT DISTINCT` removes
  the replays. When the alert ends, its end time is a new row. A replay
  with other audiences is also a new row, because `audiences` is part of
  the key. The derived phase reads the latest state.
- Errors: `md5` keeps the key small for long names, bodies, and tag sets.
  Every column of an alert or an error item except the record times is in
  its key, so a copy of a key is always a duplicate.
- Vehicle: the writer selects the VIN first and inserts only a VIN that it
  does not find. An `INSERT ... ON CONFLICT DO NOTHING` takes an identity
  value also when it inserts nothing, and the ID is a `smallint`. The
  writer keeps the IDs after the commit.
- Connectivity: the server sets `connection_id` from the request header
  `X-TXID` when that header is a UUID, and makes a random UUID only
  otherwise
  ([`server/streaming/socket.go#L105-L128`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/server/streaming/socket.go#L105-L128)).
  No source says whether the vehicle uses a new `X-TXID` for each
  connection. The key therefore includes `created_at` (server clock, in
  microseconds). A reused ID then makes a new row, not a dropped event.
  Only a retry of the same insert matches an existing row. Connectivity
  events are not resent, because the server creates them once.

### Write batching

A flush (ADR 0001: oldest row 15 minutes, 5000 signal rows, or shutdown)
is normally one transaction:

1. Find or insert the vehicle row. The process caches the ID.
2. One `INSERT ... SELECT FROM UNNEST(...)` statement for each table with
   rows, with bound arrays.
3. Commit.

A transaction holds at most 5000 signal rows and at most 8 MiB of message
bytes (topic and payload; records without signal rows count here only).
The writer splits a larger batch into more transactions; a record is never
split, so a larger record is a transaction of its own. After a data error
(SQLSTATE class 22 or 23) the writer splits the batch into halves, each in
its own transaction, the first half first, to isolate the bad record (ADR
0001); the first copy of a key still wins. Signal and connectivity rows go
in key order, so two writers with the same keys take their locks in the
same order. Alert, error, and unprocessed rows keep the order of the batch;
a deadlock of two writers (SQLSTATE 40P01) is an answered failure, and the
caller retries. Each transaction has a client deadline of 90 s, also for a
connection that goes silent. A failed write tells the caller how many
records at the start of the batch are stored. Queue items leave memory
only after their rows commit.

### Expected size

Estimate for `raw.vehicle_signal` with a numeric value (PostgreSQL page and
tuple layout):

| Part                        | Bytes for each row |
| --------------------------- | -----------------: |
| Heap tuple: header 32 B, data 32 B, line pointer 4 B | 68 |
| Primary key entry, 28 B at about 70 % leaf fill       | 40 |
| BRIN index                                            | below 1 |
| **Total**                                             | **about 110** |

A location row is 8 B larger. A short text value is about the same size.

The signal counts come from the usage estimate of the tuned vehicle config
([`docs/telemetry-usage-estimate.md`](../telemetry-usage-estimate.md)).
The estimate uses assumed hours, so the counts are provisional. The owner
limit is 450,000 signals (3 USD) in any case.

| Case                                      | Signals each month | Storage each month | Time to 1 GB  |
| ----------------------------------------- | -----------------: | -----------------: | ------------- |
| Planning estimate                         | 134,886            | about 15 MB        | about 5 years |
| Worst case (awake all month)              | 304,529            | about 34 MB        | about 2.5 years |
| Owner limit                               | 450,000            | about 50 MB        | about 20 months |

The other tables add less than 1 MB each month. These numbers are
estimates.

Measured on 26 November 2026 (PostgreSQL 18.7 on the Neon branch `test`,
test `measure_the_size_of_a_signal_row` in
`crates/ingest/tests/storage/size.rs`): 100,000 signal rows in 100
flushes of 1,000 rows, 70 % numbers, 10 % locations, 10 % booleans, 10 %
short text, after `VACUUM ANALYZE`.

| Part                                   | Bytes for each row |
| -------------------------------------- | -----------------: |
| Tuple (`pg_column_size`), numeric row  | 65                 |
| Tuple, location row                    | 73                 |
| Heap (`pg_relation_size`)              | 76.6               |
| Primary key                            | 34.2               |
| BRIN index                             | 0.25               |
| **Total (`pg_total_relation_size`)**   | **111.7**          |

The total matches the estimate of about 110 B. The heap of the same rows
with `field smallint` is 68.3 B for each row, so `integer` costs 8.3 B
(about 7 % of the total). At the owner limit of 450,000 signals, that is
about 3.7 MB more each month.

### Roles and privileges

Two roles are created with SQL, so neither is a member of
`neon_superuser`. `STACK.md` §15 records owner authority for "an
application role with limited privileges". The lead decided on
26 November 2026, within delegated authority, that the second role
`telemetry_migrator` serves the same intent: it is a least-privilege
migration role, and it keeps `neondb_owner` out of Railway. The lead
reports this decision to the owner.

| Role                  | Used by                                | Privileges                                                                                   |
| --------------------- | -------------------------------------- | -------------------------------------------------------------------------------------------- |
| `telemetry_migrator`  | `ingest migrate` (pre-deploy command)   | Owns schema `raw` and its objects. `CONNECT` on `neondb`. Nothing outside `raw`. `search_path` `raw`. |
| `telemetry_ingest`    | `ingest run`                            | `CONNECT` on `neondb`. `USAGE` on schema `raw`. `SELECT`, `INSERT` on the `raw` tables and on `raw._sqlx_migrations` (`SELECT` only). No privilege on the identity sequences: an `INSERT` into an identity column needs none (tested), and `nextval()` would let the role use up the `smallint` vehicle IDs. No `UPDATE`, `DELETE`, `TRUNCATE`, or DDL. `CONNECTION LIMIT 5` (two deployments with 2 connections each can overlap during a deploy, plus one spare), `statement_timeout` 30 s, `search_path` `raw`. |

PUBLIC keeps the PostgreSQL defaults: `CONNECT` and `TEMPORARY` on the
database, and `USAGE` on schema `public`, where PUBLIC cannot create. So
`telemetry_ingest` can create temporary tables in its own session; it
cannot create objects in `raw` or `public`.

- `neondb_owner` creates the two roles and the schema once, with
  `db/roles.sql` (`README.md`, "Database roles"). It is never a Railway
  variable. The script is idempotent. It needs the `SET` option on
  `telemetry_migrator` to create a schema for it (PostgreSQL 16 and
  later), takes it for the run, and gives it back. It takes an advisory
  lock, so two runs at the same time wait for each other instead of
  failing on the privileges of the database.
- The script sets the passwords as plain text. Neon refuses a SCRAM
  verifier that the client computes ("Neon only supports being given
  plaintext passwords", SQLSTATE XX000, tested), because the compute hands
  each password to the Neon control plane. A server log line or an error
  context of the `ALTER ROLE` statement can therefore show a password on
  the Neon side. After a suspected leak, run the script again with new
  passwords and update the Railway variables.
- The script sets default privileges for `telemetry_migrator` in `raw`:
  each table that the migrator creates gives `telemetry_ingest` `SELECT`
  and `INSERT`. The migrations therefore name no role. Migration 0001
  takes `INSERT` on `raw._sqlx_migrations` back, because SQLx creates that
  table before the first migration.
- Railway variables: `DATABASE_URL` (`telemetry_ingest`) and
  `MIGRATION_DATABASE_URL` (`telemetry_migrator`). `ingest run` reads only
  the first, `ingest migrate` only the second. Both use
  `sslmode=verify-full`.
- The child process `fleet-telemetry` gets a cleared environment with only
  the values it needs.
- Accepted risk: the pre-deploy command needs the migrator URL, and Railway
  gives the same variables to the running container [R]. A compromised
  `ingest` process can therefore change or drop tables in `raw`. It cannot
  touch roles or other schemas. Neon keeps 6 hours of restore history [N1].
  Reassessment: when Railway can give a variable to the pre-deploy command
  only, when the migration step can run outside the service, or when a
  security finding affects the running container.

### Migrations

- SQLx migrations in `migrations/` at the repository root, numbered and
  forward-only.
- `ingest migrate` applies them with SQLx's embedded migrator as the
  Railway pre-deploy command (ADR 0001). A failure stops the deployment and
  the old version keeps running.
- `ingest run` never migrates. At start, it checks that every migration
  that the binary embeds is applied with the same checksum. A missing or
  changed migration is an answered failure: the gate stays closed, also
  after 120 s, and `/health` fails. Only a database with no answer leads
  to degraded mode (ADR 0001, gate condition 3). Migrations that the binary
  does not know are allowed, also for `ingest migrate`, so that the
  pre-deploy command of an earlier release succeeds after a later release.
- The SQL names no schema. Both commands set the `search_path` to `raw`
  only. `ingest migrate` first checks that `raw` is the current schema,
  so a missing schema fails before any statement. The tests use a schema
  of their own in the same way.
- Each migration must work with the previous release (expand and contract).
  A destructive step waits for a later release and needs the owner's
  approval when it deletes data.

### Test databases

- Tests that need PostgreSQL use the Neon branch `test` of project
  `telemetry`. The lead created it on 26 November 2026 from `production`,
  before `production` held vehicle data, with a fixed compute of 0.25 CU.
  Never reset it from its parent, because that copies production data.
- The test branch has the same PostgreSQL major version as production.
- Each test uses a schema of its own, and roles of its own where it needs
  roles, named `itest_<unix seconds>_<random>`. The test removes them, also
  after a failure. A later run removes the objects of a killed run after
  6 hours. Two runs can therefore share the branch at the same time. The
  tests refuse the production endpoint and a database with a schema
  `raw`. The branch counts against the 1 GB of the project.
- The test compute draws from the same 100 CU-hours as production. Budget:
  at most 20 active hours (5 CU-hours) each month (ADR 0001). Tests close
  their connections when they end, so the compute scales to zero after 5
  minutes. No process keeps a connection open between runs.
- The test credentials stay outside the repository.

## Key alternatives

- **One row for each record with all datums in `jsonb`.** Fewer rows, but
  JSON loses the `int64` and `float` distinction, makes time series
  queries slower, and has no per-field idempotency. Rejected.
- **One `double precision` column for every number.** Loses the variant and
  the precision of `int64`. Rejected.
- **Keep the raw payload bytes of every record.** Perfect for reprocessing,
  but it roughly doubles the storage. Kept only for unprocessed records.
- **A key with the transaction ID.** ZMQ does not carry it. Not possible.
- **Text names for `field`.** Readable, but names can change and unknown
  numbers have no name. Rejected; the number is the stable protobuf
  identity.
- **The Neon owner role for the running process.** Too broad. Rejected.
- **Migrations at process start.** `STACK.md` §5 forbids it. Rejected.

## Consequences

- Queries in the derived phase map field and enum numbers to names with the
  vendored `.proto` files.
- A conflicting duplicate or an unknown variant needs a later reprocessing
  step. The data stays available in `raw.unprocessed_record`.
- The raw tables grow without a deletion job. `VISION.md` still lists a
  retention assumption of 90 days. The owner decides the retention rule.
- The privacy table of `VISION.md` does not yet list the two database
  credentials in the Railway variables. `VISION.md` is the owner's file.
- `raw.ingest_event` makes the gaps of the ingest process visible next to
  the vehicle's own gaps.
- The stored history has no backup beyond the 6 hours of Neon restore
  history [N1]. A wrong migration, an operator error, or a compromised
  migrator credential found later than 6 hours cannot be undone. The lead
  reports this to the owner together with the role decision.

## Cost of reversal

Low while the tables are small. A changed model needs a migration and a
backfill from the existing rows. The original bytes exist only for
unprocessed records, so a backfill cannot recover a value that the current
model did not keep.

## Reassessment

- The database size passes 600 MB, or 12 months of data exist.
- The owner sets the raw retention rule.
- Count of `conflicting_duplicate` rows above zero, or many
  `unknown_value_variant` or `unknown_fields` rows after a firmware update.
- The derived phase needs other indexes or a compact copy of old data.

## Not verified

- That the vehicle always sets `created_at` on `VehicleAlerts` and
  `VehicleErrors`. No source states it. Rows in `raw.unprocessed_record`
  with reason `missing_created_at` and topic `alerts` or `errors` show a
  violation.
- That the vehicle never sends two different values for one field with the
  same `created_at`. The `conflicting_duplicate` count shows it.
- Whether the vehicle reuses an `X-TXID` connection ID. The key does not
  depend on it.
- That Neon counts child branches against the 1 GB of the project, and the
  SQLSTATE of a write to a full Neon storage. The storage change could not
  test either without filling the branch; the writer retries every error
  that is not a data error.
- The production `sslmode`, `PGSSLMODE`, and the certificate version
  (`STACK.md` §5). The storage forces `sslmode=verify-full`, and the
  certificate of the test branch verifies (tested). The production
  endpoint was not contacted; the lead checks it read-only at release.

## Sources

- [P] `protos/vehicle_data.proto`, tag `v0.10.1`: <https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/protos/vehicle_data.proto#L752-L834>, and the other files in [`protos/`](https://github.com/teslamotors/fleet-telemetry/tree/6689adb831691b1de211b4aa8220babf7309f31c/protos).
- [T] Tesla Fleet Telemetry: <https://developer.tesla.com/docs/fleet-api/fleet-telemetry>
- [N1] Neon plans: <https://neon.com/docs/introduction/plans>
- [N2] Neon roles, "Manage roles with SQL": <https://neon.com/docs/manage/roles>
- [PG1] PostgreSQL 18 `INSERT`: <https://www.postgresql.org/docs/18/sql-insert.html>
- [PG2] PostgreSQL 18 `CREATE TABLE`: <https://www.postgresql.org/docs/18/sql-createtable.html>
- [R] Railway pre-deploy command: <https://docs.railway.com/deployments/pre-deploy-command>
