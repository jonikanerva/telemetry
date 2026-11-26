-- The raw tables of ADR 0002 ("Schema and tables", "Value representation",
-- "Time columns", "Idempotency").
--
-- Schema: the statements name no schema. `ingest migrate` sets the
-- `search_path` to `raw` only (`crates/ingest/src/storage/connect.rs`), so
-- every object goes into `raw`. The tests use a schema of their own in the
-- same way. When the schema does not exist, PostgreSQL has no schema to
-- create in and the migration fails (SQLSTATE 3F000).
--
-- Privileges: this migration names no role. `db/roles.sql` sets default
-- privileges for the migrator role, so the ingest role gets SELECT and
-- INSERT on each table that the migrator creates in the schema. The last
-- statement takes INSERT on `_sqlx_migrations` back.
--
-- Keys: a unique key that holds text of unknown length uses md5() of the
-- value. A btree entry has a size limit, and a longer key would fail with
-- SQLSTATE 54000, which the writer retries without end. The md5() and the
-- length limits below keep every key small, so only data errors (classes
-- 22 and 23) depend on the values.

-- One row for each VIN. The VIN is stored only here. 1 to 64 bytes: the
-- certificate common name that `fleet-telemetry` reads the VIN from has
-- at most 64 characters (X.509 ub-common-name). An empty VIN is not an
-- identity.
CREATE TABLE vehicle (
    id smallint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    vin text NOT NULL,
    first_seen_at timestamptz NOT NULL,
    CONSTRAINT vehicle_vin_key UNIQUE (vin),
    CONSTRAINT vehicle_vin_length CHECK (octet_length(vin) BETWEEN 1 AND 64)
);

-- One row for each datum of a `V` record. The columns are in order of
-- alignment: 8-byte values, then `field`, then the 2-byte and 1-byte
-- values, then the values of variable length.
CREATE TABLE vehicle_signal (
    created_at timestamptz NOT NULL,
    received_at timestamptz NOT NULL,
    int_value bigint,
    float_value double precision,
    latitude double precision,
    longitude double precision,
    -- The number of the `Field` enum, as sent. The wire type is int32.
    field integer NOT NULL,
    vehicle_id smallint NOT NULL REFERENCES vehicle (id),
    -- The protobuf field number of the `Value` variant.
    value_type smallint NOT NULL,
    is_resend boolean NOT NULL,
    bool_value boolean,
    text_value text,
    json_value jsonb,
    CONSTRAINT vehicle_signal_pkey PRIMARY KEY (vehicle_id, field, created_at),
    CONSTRAINT vehicle_signal_value_type CHECK (value_type > 0),
    CONSTRAINT vehicle_signal_one_value CHECK (
        num_nonnulls(int_value, float_value, latitude, bool_value, text_value, json_value) = 1
    ),
    CONSTRAINT vehicle_signal_location CHECK ((latitude IS NULL) = (longitude IS NULL)),
    -- ADR 0002 "Value representation": the column follows from the variant.
    CONSTRAINT vehicle_signal_value_column CHECK (
        CASE
            WHEN value_type = 1 THEN text_value IS NOT NULL
            WHEN value_type IN (2, 3) THEN int_value IS NOT NULL
            WHEN value_type IN (4, 5) THEN float_value IS NOT NULL
            WHEN value_type IN (6, 10) THEN bool_value IS NOT NULL
            WHEN value_type = 7 THEN latitude IS NOT NULL
            WHEN value_type IN (21, 31, 39) THEN coalesce(jsonb_typeof(json_value) = 'object', false)
            ELSE int_value IS NOT NULL
        END
    )
);

CREATE INDEX vehicle_signal_created_at_brin ON vehicle_signal USING brin (created_at);

-- One row for each distinct alert state. An active alert has `ended_at`
-- NULL; NULLS NOT DISTINCT makes a replay of it equal.
CREATE TABLE vehicle_alert (
    vehicle_id smallint NOT NULL REFERENCES vehicle (id),
    name text NOT NULL,
    -- The numbers of the `Audience` enum, in the order of the record. The
    -- enum has 4 values; 64 numbers keep the key below the btree limit.
    audiences integer[] NOT NULL,
    started_at timestamptz,
    ended_at timestamptz,
    -- `VehicleAlerts.created_at`.
    created_at timestamptz NOT NULL,
    received_at timestamptz NOT NULL,
    CONSTRAINT vehicle_alert_audiences CHECK (cardinality(audiences) <= 64)
);

CREATE UNIQUE INDEX vehicle_alert_key ON vehicle_alert (
    vehicle_id, md5(name), started_at, ended_at, audiences
) NULLS NOT DISTINCT;

-- One row for each error item.
CREATE TABLE vehicle_error (
    vehicle_id smallint NOT NULL REFERENCES vehicle (id),
    -- `VehicleError.created_at`; NULL when the item has none.
    created_at timestamptz,
    name text NOT NULL,
    tags jsonb NOT NULL,
    body text NOT NULL,
    -- `VehicleErrors.created_at`.
    record_created_at timestamptz NOT NULL,
    received_at timestamptz NOT NULL,
    CONSTRAINT vehicle_error_tags CHECK (jsonb_typeof(tags) = 'object')
);

CREATE UNIQUE INDEX vehicle_error_key ON vehicle_error (
    vehicle_id, created_at, md5(name), md5(tags::text), md5(body)
) NULLS NOT DISTINCT;

-- One row for each connect or disconnect event of the server.
CREATE TABLE connectivity_event (
    connection_id uuid NOT NULL,
    -- The server clock.
    created_at timestamptz NOT NULL,
    received_at timestamptz NOT NULL,
    -- The number of the `ConnectivityEvent` enum, as sent.
    status integer NOT NULL,
    vehicle_id smallint NOT NULL REFERENCES vehicle (id),
    -- As sent, also when empty.
    network_interface text NOT NULL,
    CONSTRAINT connectivity_event_pkey PRIMARY KEY (connection_id, status, created_at)
);

-- One row for each message and reason that the tables above cannot hold
-- as rows. `topic` and `payload` are the original ZMQ frames as bytes: a
-- topic can be any bytes, also bytes that are not UTF-8 or a NUL byte,
-- which `text` cannot hold.
--
-- A new reason needs a migration that adds it here. The pre-deploy
-- command applies it before the new binary runs, and `ingest run` does
-- not start before every migration that it embeds is applied.
CREATE TABLE unprocessed_record (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    received_at timestamptz NOT NULL,
    topic bytea NOT NULL,
    payload bytea NOT NULL,
    reason text NOT NULL,
    -- The SQLSTATE of the rejection, for reason `database_rejected` only.
    sqlstate text,
    CONSTRAINT unprocessed_record_reason CHECK (reason IN (
        'decode_failed',
        'unknown_topic',
        'missing_created_at',
        'unknown_value_variant',
        'unknown_fields',
        'conflicting_duplicate',
        'database_rejected'
    )),
    CONSTRAINT unprocessed_record_sqlstate CHECK (
        (reason = 'database_rejected') = (sqlstate IS NOT NULL)
        AND sqlstate ~ '^[0-9A-Z]{5}$'
    )
);

CREATE UNIQUE INDEX unprocessed_record_key ON unprocessed_record (
    md5(topic), reason, md5(payload)
);

-- One row for each event of the ingest process.
CREATE TABLE ingest_event (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- Random for each process start.
    run_id uuid NOT NULL,
    occurred_at timestamptz NOT NULL,
    kind text NOT NULL,
    reason text,
    -- The count of the window. bigint holds every unsigned 32-bit count.
    dropped_count bigint,
    first_received_at timestamptz,
    last_received_at timestamptz,
    CONSTRAINT ingest_event_key UNIQUE (run_id, kind, occurred_at),
    CONSTRAINT ingest_event_kind CHECK (kind IN (
        'started',
        'stopped',
        'gate_closed',
        'gate_opened',
        'degraded_mode',
        'child_exited',
        'records_dropped',
        'zmq_shortfall',
        'gate_full'
    )),
    -- The three counted kinds have the count and both times; the other
    -- kinds have none of them.
    CONSTRAINT ingest_event_window CHECK (
        CASE
            WHEN kind IN ('records_dropped', 'zmq_shortfall', 'gate_full')
                THEN num_nonnulls(dropped_count, first_received_at, last_received_at) = 3
            ELSE num_nonnulls(dropped_count, first_received_at, last_received_at) = 0
        END
    ),
    CONSTRAINT ingest_event_count CHECK (dropped_count > 0),
    CONSTRAINT ingest_event_window_order CHECK (first_received_at <= last_received_at)
);

-- The ingest role may read the migration history, but it must not write it
-- (ADR 0002, "Roles and privileges"). The default privileges of
-- `db/roles.sql` also gave it INSERT on `_sqlx_migrations`, which SQLx
-- creates before this migration. Take back every privilege except SELECT
-- from each role that is not the owner.
DO $$
DECLARE
    grantee regrole;
BEGIN
    FOR grantee IN
        SELECT DISTINCT privilege.grantee::regrole
        FROM pg_class AS class, aclexplode(class.relacl) AS privilege
        WHERE class.oid = '_sqlx_migrations'::regclass
          AND privilege.grantee <> class.relowner
          AND privilege.grantee <> 0
          AND privilege.privilege_type <> 'SELECT'
    LOOP
        EXECUTE format(
            'REVOKE INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER, MAINTAIN ON _sqlx_migrations FROM %s',
            grantee
        );
    END LOOP;
END
$$;
