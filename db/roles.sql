-- Roles, schema, and privileges of ADR 0002 ("Roles and privileges").
--
-- Run this script as the Neon owner role `neondb_owner`, in the database
-- `neondb`, before the first `ingest migrate`. The script is idempotent:
-- a second run creates nothing new and sets the same privileges again.
-- `README.md` ("Database roles") gives the procedure.
--
-- Parameters are session settings. The script reads them with
-- current_setting(), so no value is written into the SQL text:
--
--   telemetry.ingest_password    Password of the ingest role. Set: the
--                                role gets LOGIN and this password.
--                                Unset or empty: the script does not
--                                change the password or the LOGIN
--                                attribute of the role.
--   telemetry.migrator_password  Password of the migrator role. Same rule.
--   telemetry.web_password       Password of the web role. Same rule.
--   telemetry.ingest_role        Default `telemetry_ingest`.
--   telemetry.migrator_role      Default `telemetry_migrator`.
--   telemetry.web_role           Default `telemetry_web`.
--   telemetry.schema             Default `raw`.
--
-- Only the tests set the last four: each test uses roles and a schema of
-- its own, because roles are shared by all databases of a branch.
--
-- The passwords are plain text: Neon refuses a SCRAM verifier that a
-- client computes ("Neon only supports being given plaintext passwords",
-- SQLSTATE XX000), because the compute hands each password to the Neon
-- control plane. The client SQL text holds no password, but the server
-- can still show it in two places: the ALTER ROLE statement that the DO
-- block runs holds it (a server log line or an error context of that
-- statement), and the `set_config()` statement of the client binds it (a
-- server that logs statements with their parameters). After a suspected
-- leak, run the procedure again with new passwords.
--
-- What the script does:
--
-- 1. It creates the three roles with SQL, so no role is a member of
--    `neon_superuser`. A role gets LOGIN and its password only from a run
--    that gives its password. A run without the password of a role
--    keeps the password and the LOGIN attribute of that role; a role
--    that the run creates without a password has NOLOGIN. The other
--    steps run for all three roles in each run. The ingest role has
--    CONNECTION LIMIT 5 (a process holds at most 2 connections, one for each running
--    call: the writer and the start-up check; two processes overlap during
--    a deploy; one spare), `statement_timeout` 30 s, and `search_path` set
--    to the schema; the migrator role has the same `search_path`. The web
--    role has CONNECTION LIMIT 4 (2 concurrent requests, a deploy
--    overlap, one spare), `search_path` set to the schema,
--    `statement_timeout` 10 s, `idle_in_transaction_session_timeout` 15 s,
--    and `default_transaction_read_only` on. The read-only default is
--    only a second barrier: a session can turn it off. The privileges of
--    step 6 make the role read-only. The script first resets every
--    setting of the web role, so the role has exactly these settings.
-- 2. It grants CONNECT on the current database to the three roles.
-- 3. It creates the schema, owned by the migrator role. The migrator then
--    owns every object that `ingest migrate` creates in it.
-- 4. As the migrator role, it grants the ingest role USAGE on the schema,
--    and sets default privileges: SELECT and INSERT on each table that the
--    migrator creates in the schema. No UPDATE, DELETE, TRUNCATE, or DDL.
--    The ingest role gets no privilege on sequences: an INSERT into an
--    identity column needs none (tested), and nextval() would let it use
--    up the ID values.
-- 5. For tables that exist already (a second run after a migration), it
--    grants the same privileges, and SELECT only on `_sqlx_migrations`.
-- 6. As the migrator role, it sets the privileges of the web role in the
--    schema. First it removes every privilege of the web role on the
--    schema, its tables, its columns, and its sequences, and every
--    default privilege of the web role. So a wrong earlier grant does not
--    remain. Then it grants USAGE on the schema and SELECT on the columns
--    of `web_columns` only. Each grant is a column privilege, never a
--    table privilege, so a column that a later migration adds stays
--    unreadable. The web role reads no location: it gets nothing on
--    `vehicle_signal.latitude`, `longitude`, `json_value`, `bool_value`,
--    and `is_resend`, on `connectivity_event.network_interface` and
--    `received_at`, or on `ingest_event.id`. It
--    gets nothing on `vehicle.vin`, on `unprocessed_record` (its payload
--    bytes hold the VIN), on `vehicle_alert`, `vehicle_error`,
--    `_sqlx_migrations`, or on a sequence. Each grant is made only when
--    its table exists: on a new database the tables exist only after
--    `ingest migrate`. A new column or table that the web reads needs a
--    change of `web_columns` and a new run of the script after the
--    migration. The script runs in one transaction, so no session sees
--    a partial set of privileges.
--
-- PUBLIC keeps the PostgreSQL defaults: CONNECT and TEMPORARY on the
-- database, and USAGE on the schema `public`, where PUBLIC cannot create.
-- So the ingest role and the web role can create temporary tables in
-- their own sessions. TEMPORARY comes from PUBLIC, not from a grant to a
-- role, so a REVOKE from one role alone has no effect; only a REVOKE
-- from PUBLIC would remove it, and that changes every role of the
-- database, also later ones. The script does not do it; it is a
-- decision of the lead or the owner.
--
-- The owner role needs the SET option on the migrator role to create a
-- schema for it (PostgreSQL 16 and later). The script grants that option
-- to the current role for the run and takes it back at the end.

DO $roles$
DECLARE
    ingest_role text := coalesce(nullif(current_setting('telemetry.ingest_role', true), ''), 'telemetry_ingest');
    migrator_role text := coalesce(nullif(current_setting('telemetry.migrator_role', true), ''), 'telemetry_migrator');
    schema_name text := coalesce(nullif(current_setting('telemetry.schema', true), ''), 'raw');
    ingest_password text := nullif(current_setting('telemetry.ingest_password', true), '');
    migrator_password text := nullif(current_setting('telemetry.migrator_password', true), '');
    web_role text := coalesce(nullif(current_setting('telemetry.web_role', true), ''), 'telemetry_web');
    web_password text := nullif(current_setting('telemetry.web_password', true), '');
    -- The columns that the web role reads: one row for each table, the
    -- table name first. A column that is not listed stays unreadable,
    -- also a column that a later migration adds.
    web_columns text[] := ARRAY[
        ARRAY['vehicle_signal', 'vehicle_id, field, created_at, received_at, value_type, int_value, float_value, text_value'],
        ARRAY['connectivity_event', 'connection_id, vehicle_id, created_at, status'],
        ARRAY['ingest_event', 'run_id, occurred_at, kind, reason, dropped_count, first_received_at, last_received_at'],
        ARRAY['vehicle', 'id']
    ];
    web_grant text[];
    owner_name text;
BEGIN
    -- Two runs at the same time would change the same catalog rows (the
    -- privileges of the database) and fail with "tuple concurrently
    -- updated". The lock makes a second run wait until the first commits.
    PERFORM pg_advisory_xact_lock(hashtext('telemetry.roles'));

    -- 1. Roles.
    -- A role without a password parameter keeps its password and its
    -- LOGIN attribute. A missing role without a password parameter gets
    -- NOLOGIN and no password. So a run for one role does not change the
    -- login of the other roles.
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = migrator_role) THEN
        EXECUTE format('CREATE ROLE %I NOLOGIN', migrator_role);
    END IF;
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = ingest_role) THEN
        EXECUTE format('CREATE ROLE %I NOLOGIN', ingest_role);
    END IF;
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = web_role) THEN
        EXECUTE format('CREATE ROLE %I NOLOGIN', web_role);
    END IF;
    EXECUTE format('ALTER ROLE %I NOCREATEDB NOCREATEROLE', migrator_role);
    EXECUTE format('ALTER ROLE %I NOCREATEDB NOCREATEROLE CONNECTION LIMIT 5', ingest_role);
    EXECUTE format('ALTER ROLE %I NOCREATEDB NOCREATEROLE CONNECTION LIMIT 4', web_role);
    IF migrator_password IS NOT NULL THEN
        EXECUTE format('ALTER ROLE %I LOGIN PASSWORD %L', migrator_role, migrator_password);
    END IF;
    IF ingest_password IS NOT NULL THEN
        EXECUTE format('ALTER ROLE %I LOGIN PASSWORD %L', ingest_role, ingest_password);
    END IF;
    IF web_password IS NOT NULL THEN
        EXECUTE format('ALTER ROLE %I LOGIN PASSWORD %L', web_role, web_password);
    END IF;
    EXECUTE format('ALTER ROLE %I SET search_path = %I', migrator_role, schema_name);
    EXECUTE format('ALTER ROLE %I SET search_path = %I', ingest_role, schema_name);
    EXECUTE format('ALTER ROLE %I SET statement_timeout = %L', ingest_role, '30s');
    EXECUTE format('ALTER ROLE %I RESET ALL', web_role);
    EXECUTE format('ALTER ROLE %I SET search_path = %I', web_role, schema_name);
    EXECUTE format('ALTER ROLE %I SET statement_timeout = %L', web_role, '10s');
    EXECUTE format('ALTER ROLE %I SET idle_in_transaction_session_timeout = %L', web_role, '15s');
    EXECUTE format('ALTER ROLE %I SET default_transaction_read_only = on', web_role);

    -- 2. Database.
    EXECUTE format(
        'GRANT CONNECT ON DATABASE %I TO %I, %I, %I',
        current_database(), migrator_role, ingest_role, web_role
    );

    -- 3. Schema.
    EXECUTE format('GRANT %I TO CURRENT_USER WITH INHERIT FALSE, SET TRUE', migrator_role);
    EXECUTE format('CREATE SCHEMA IF NOT EXISTS %I AUTHORIZATION %I', schema_name, migrator_role);
    SELECT nspowner::regrole::text INTO owner_name FROM pg_namespace WHERE nspname = schema_name;
    IF owner_name IS DISTINCT FROM quote_ident(migrator_role) THEN
        RAISE EXCEPTION 'schema % has owner %, expected %', schema_name, owner_name, migrator_role;
    END IF;

    -- 4., 5., and 6. Privileges, as the owner of the schema.
    EXECUTE format('SET LOCAL ROLE %I', migrator_role);
    EXECUTE format('GRANT USAGE ON SCHEMA %I TO %I', schema_name, ingest_role);
    EXECUTE format(
        'ALTER DEFAULT PRIVILEGES IN SCHEMA %I GRANT SELECT, INSERT ON TABLES TO %I',
        schema_name, ingest_role
    );
    EXECUTE format('GRANT SELECT, INSERT ON ALL TABLES IN SCHEMA %I TO %I', schema_name, ingest_role);
    IF to_regclass(format('%I._sqlx_migrations', schema_name)) IS NOT NULL THEN
        EXECUTE format('REVOKE INSERT ON %I._sqlx_migrations FROM %I', schema_name, ingest_role);
    END IF;
    -- 6. A REVOKE on a table also removes the column privileges of that
    -- table. Each REVOKE removes only the grants of the migrator role,
    -- which owns every object in the schema.
    EXECUTE format('REVOKE ALL ON SCHEMA %I FROM %I', schema_name, web_role);
    EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA %I REVOKE ALL ON TABLES FROM %I', schema_name, web_role);
    EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA %I REVOKE ALL ON SEQUENCES FROM %I', schema_name, web_role);
    EXECUTE format('REVOKE ALL ON ALL TABLES IN SCHEMA %I FROM %I', schema_name, web_role);
    EXECUTE format('REVOKE ALL ON ALL SEQUENCES IN SCHEMA %I FROM %I', schema_name, web_role);
    EXECUTE format('GRANT USAGE ON SCHEMA %I TO %I', schema_name, web_role);
    -- Column privileges only. The column lists are constants of this
    -- script, so they go into the statement as text.
    FOREACH web_grant SLICE 1 IN ARRAY web_columns LOOP
        IF to_regclass(format('%I.%I', schema_name, web_grant[1])) IS NOT NULL THEN
            EXECUTE format(
                'GRANT SELECT (%s) ON %I.%I TO %I',
                web_grant[2], schema_name, web_grant[1], web_role
            );
        END IF;
    END LOOP;
    RESET ROLE;
    EXECUTE format('REVOKE %I FROM CURRENT_USER', migrator_role);
END
$roles$;
