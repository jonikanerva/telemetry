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
--   telemetry.ingest_password    Password of the ingest role. Unset or
--                                empty: the password does not change.
--   telemetry.migrator_password  Password of the migrator role. Same rule.
--   telemetry.ingest_role        Default `telemetry_ingest`.
--   telemetry.migrator_role      Default `telemetry_migrator`.
--   telemetry.schema             Default `raw`.
--
-- Only the tests set the last three: each test uses roles and a schema of
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
-- 1. It creates both roles with SQL, so neither role is a member of
--    `neon_superuser`. Both can log in. The ingest role has CONNECTION
--    LIMIT 5 (a process holds at most 2 connections, one for each running
--    call: the writer and the start-up check; two processes overlap during
--    a deploy; one spare), `statement_timeout` 30 s, and `search_path` set
--    to the schema; the migrator role has the same `search_path`.
-- 2. It grants CONNECT on the current database to both roles.
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
--
-- PUBLIC keeps the PostgreSQL defaults: CONNECT and TEMPORARY on the
-- database, and USAGE on the schema `public`, where PUBLIC cannot create.
-- So the ingest role can create temporary tables in its own session.
-- TEMPORARY comes from PUBLIC, not from a grant to the role, so a REVOKE
-- from the ingest role alone has no effect; only a REVOKE from PUBLIC
-- would remove it, and that changes every role of the database, also
-- later ones. The script does not do it; it is a decision of the lead or
-- the owner.
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
    owner_name text;
BEGIN
    -- Two runs at the same time would change the same catalog rows (the
    -- privileges of the database) and fail with "tuple concurrently
    -- updated". The lock makes a second run wait until the first commits.
    PERFORM pg_advisory_xact_lock(hashtext('telemetry.roles'));

    -- 1. Roles.
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = migrator_role) THEN
        EXECUTE format('CREATE ROLE %I LOGIN', migrator_role);
    END IF;
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = ingest_role) THEN
        EXECUTE format('CREATE ROLE %I LOGIN', ingest_role);
    END IF;
    EXECUTE format('ALTER ROLE %I LOGIN NOCREATEDB NOCREATEROLE', migrator_role);
    EXECUTE format('ALTER ROLE %I LOGIN NOCREATEDB NOCREATEROLE CONNECTION LIMIT 5', ingest_role);
    IF migrator_password IS NOT NULL THEN
        EXECUTE format('ALTER ROLE %I PASSWORD %L', migrator_role, migrator_password);
    END IF;
    IF ingest_password IS NOT NULL THEN
        EXECUTE format('ALTER ROLE %I PASSWORD %L', ingest_role, ingest_password);
    END IF;
    EXECUTE format('ALTER ROLE %I SET search_path = %I', migrator_role, schema_name);
    EXECUTE format('ALTER ROLE %I SET search_path = %I', ingest_role, schema_name);
    EXECUTE format('ALTER ROLE %I SET statement_timeout = %L', ingest_role, '30s');

    -- 2. Database.
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO %I, %I', current_database(), migrator_role, ingest_role);

    -- 3. Schema.
    EXECUTE format('GRANT %I TO CURRENT_USER WITH INHERIT FALSE, SET TRUE', migrator_role);
    EXECUTE format('CREATE SCHEMA IF NOT EXISTS %I AUTHORIZATION %I', schema_name, migrator_role);
    SELECT nspowner::regrole::text INTO owner_name FROM pg_namespace WHERE nspname = schema_name;
    IF owner_name IS DISTINCT FROM quote_ident(migrator_role) THEN
        RAISE EXCEPTION 'schema % has owner %, expected %', schema_name, owner_name, migrator_role;
    END IF;

    -- 4. and 5. Privileges, as the owner of the schema.
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
    RESET ROLE;
    EXECUTE format('REVOKE %I FROM CURRENT_USER', migrator_role);
END
$roles$;
