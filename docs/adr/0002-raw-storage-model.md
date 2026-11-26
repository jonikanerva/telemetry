# ADR 0002: Raw storage model

- Status: Accepted, 26 November 2026. Issue #4. The lead decided the open
  points within delegated authority on 26 November 2026 (ADR 0001, section
  "Lead decisions").
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
  (ADR 0001).
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
| `raw.vehicle_alert`       | each distinct alert state              | `(vehicle_id, name, started_at, ended_at, audiences)` `NULLS NOT DISTINCT`    |
| `raw.vehicle_error`       | each error item                        | unique index `(vehicle_id, created_at, name, md5(tags::text), md5(body))` `NULLS NOT DISTINCT` |
| `raw.connectivity_event`  | each connect or disconnect             | primary key `(connection_id, status, created_at)`                              |
| `raw.unprocessed_record`  | each record that cannot be stored as rows | unique index `(topic, reason, md5(payload))`                                |
| `raw.ingest_event`        | each event of the ingest process       | `(run_id, kind, occurred_at)`                                                  |

Columns:

- `raw.vehicle`: `id smallint` identity, `vin text NOT NULL UNIQUE`,
  `first_seen_at timestamptz NOT NULL`. The VIN is stored only here.
- `raw.vehicle_signal`, in this order for alignment:
  `created_at timestamptz NOT NULL` (`Payload.created_at`),
  `received_at timestamptz NOT NULL`, `int_value bigint`,
  `float_value double precision`, `latitude double precision`,
  `longitude double precision`, `vehicle_id smallint NOT NULL` (foreign key),
  `field smallint NOT NULL` (number of the `Field` enum),
  `value_type smallint NOT NULL` (field number of the `Value` variant),
  `is_resend boolean NOT NULL`, `bool_value boolean`, `text_value text`,
  `json_value jsonb`.
- `raw.vehicle_alert`: `vehicle_id`, `name text NOT NULL`,
  `audiences smallint[] NOT NULL` (enum numbers), `started_at timestamptz`,
  `ended_at timestamptz`, `created_at timestamptz` (of `VehicleAlerts`),
  `received_at timestamptz NOT NULL`.
- `raw.vehicle_error`: `vehicle_id`, `created_at timestamptz` (of the item),
  `name text NOT NULL`, `tags jsonb NOT NULL`, `body text NOT NULL`,
  `record_created_at timestamptz` (of `VehicleErrors`),
  `received_at timestamptz NOT NULL`.
- `raw.connectivity_event`: `vehicle_id`, `connection_id uuid NOT NULL`,
  `status smallint NOT NULL` (enum number), `network_interface text NOT NULL`
  (as sent, also when empty), `created_at timestamptz NOT NULL` (server
  clock), `received_at timestamptz NOT NULL`.
- `raw.unprocessed_record`: `id bigint` identity, `topic text NOT NULL`,
  `payload bytea NOT NULL` (the original protobuf bytes),
  `reason text NOT NULL`, `sqlstate text`, `received_at timestamptz NOT NULL`.
  `reason` is one of `decode_failed`, `unknown_topic`, `missing_created_at`,
  `unknown_value_variant`, `conflicting_duplicate`, `database_rejected`.
- `raw.ingest_event`: `id bigint` identity, `run_id uuid NOT NULL` (random
  for each process start), `occurred_at timestamptz NOT NULL`,
  `kind text NOT NULL` (`started`, `stopped`, `gate_closed`, `gate_opened`,
  `degraded_mode`, `child_exited`, `records_dropped`, `zmq_shortfall`,
  `gate_full`),
  `reason text`, `dropped_count integer`,
  `first_received_at timestamptz`, `last_received_at timestamptz`. A
  `records_dropped`, `zmq_shortfall`, or `gate_full` row must have the
  count and both times (CHECK). For `zmq_shortfall` and `gate_full`, the
  times are the window of the count (`gate_full` counts refused
  connections, at most one row each minute).

Indexes: the keys above, and one BRIN index on
`raw.vehicle_signal (created_at)`. The derived phase adds the indexes it
needs.

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
  `created_at` goes to `raw.unprocessed_record` as a whole.
- Text that PostgreSQL rejects (for example a NUL byte) is caught before the
  insert and stored as unprocessed with reason `database_rejected`.

### Time columns

- `created_at` is the instant from the record (`google.protobuf.Timestamp`).
  It is the time of the event. PostgreSQL keeps microseconds, so the ingest
  process truncates nanoseconds to microseconds.
- `received_at` is the clock of the ingest process when it reads the ZMQ
  message. It is not the receive time of `fleet-telemetry`, which ZMQ does
  not carry. For resent data, it can be much later than `created_at`.
- A record without `created_at` is never given an invented time.

### Idempotency

Every insert uses `ON CONFLICT DO NOTHING`. A retry after a lost commit
acknowledgement is safe.

- Signals: the key excludes `is_resend` and `received_at`, because these
  differ between copies of the same datum. The first stored copy wins.
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
- Errors: `md5` keeps the key small for long bodies and tag sets.
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

A transaction holds at most 5000 signal rows. The flush splits a larger
queue into more transactions. After a data error (SQLSTATE class 22 or 23)
the writer splits the batch into halves, each in its own transaction, to
isolate the bad record (ADR 0001). Queue items leave memory only after
their rows commit.

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
estimates. The storage PR measures `pg_total_relation_size` after a known
number of rows.

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
| `telemetry_migrator`  | `ingest migrate` (pre-deploy command)   | Owns schema `raw` and its objects. `CONNECT` on `neondb`. Nothing outside `raw`.             |
| `telemetry_ingest`    | `ingest run`                            | `CONNECT` on `neondb`. `USAGE` on schema `raw`. `SELECT`, `INSERT` on the `raw` tables and on `raw._sqlx_migrations` (`SELECT` only). `USAGE` on the identity sequences. No `UPDATE`, `DELETE`, `TRUNCATE`, or DDL. `CONNECTION LIMIT 5` (two deployments with 2 connections each can overlap during a deploy, plus one spare), `statement_timeout` 30 s, `search_path` `raw`. |

- `neondb_owner` creates the two roles and the schema once. It is never a
  Railway variable.
- Each migration that adds a table also grants the `telemetry_ingest`
  privileges for that table.
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
  does not know are allowed.
- Each migration must work with the previous release (expand and contract).
  A destructive step waits for a later release and needs the owner's
  approval when it deletes data.

### Test databases

- Tests that need PostgreSQL use the Neon branch `test` of project
  `telemetry`. The lead created it on 26 November 2026 from `production`,
  before `production` held vehicle data, with a fixed compute of 0.25 CU.
  Never reset it from its parent, because that copies production data.
- The test branch has the same PostgreSQL major version as production.
- Tests clean up their data. The branch counts against the 1 GB of the
  project.
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
  `unknown_value_variant` rows after a firmware update.
- The derived phase needs other indexes or a compact copy of old data.

## Not verified

- The size per row. The storage PR measures it.
- That the vehicle never sends two different values for one field with the
  same `created_at`. The `conflicting_duplicate` count shows it.
- Whether `INSERT` into an identity column needs `USAGE` on its sequence.
  The grant is harmless; the storage PR tests the role.
- That the rustls set-up of sqlx verifies the Neon certificate with
  `sslmode=verify-full` (`STACK.md` §5).
- Whether the vehicle reuses an `X-TXID` connection ID. The key does not
  depend on it.
- That Neon counts child branches against the 1 GB of the project, and the
  SQLSTATE of a write to a full Neon storage. The storage PR records both
  where it can; the writer retries every error that is not a data error.

## Sources

- [P] `protos/vehicle_data.proto`, tag `v0.10.1`: <https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/protos/vehicle_data.proto#L752-L834>, and the other files in [`protos/`](https://github.com/teslamotors/fleet-telemetry/tree/6689adb831691b1de211b4aa8220babf7309f31c/protos).
- [T] Tesla Fleet Telemetry: <https://developer.tesla.com/docs/fleet-api/fleet-telemetry>
- [N1] Neon plans: <https://neon.com/docs/introduction/plans>
- [N2] Neon roles, "Manage roles with SQL": <https://neon.com/docs/manage/roles>
- [PG1] PostgreSQL 18 `INSERT`: <https://www.postgresql.org/docs/18/sql-insert.html>
- [PG2] PostgreSQL 18 `CREATE TABLE`: <https://www.postgresql.org/docs/18/sql-createtable.html>
- [R] Railway pre-deploy command: <https://docs.railway.com/deployments/pre-deploy-command>
