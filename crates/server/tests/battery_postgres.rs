//! Database tests of the PostgreSQL source of the battery view.
//!
//! Each test makes its own schema and roles with `db/roles.sql` (as the
//! production procedure does), applies `migrations/` as the migrator
//! role, writes rows as the migrator role, and reads them as the web
//! role. The tests compare the snapshot of the SQL with the reference
//! rules of `telemetry_web_domain` on the same rows, so the fake source
//! of the browser tests and production give the same view.
//!
//! The tests need `TEST_DATABASE_URL`: the owner role of the test server
//! that `scripts/test-postgres.sh` starts (`mise run test`, or CI). A
//! missing URL fails the test.

//!
//! The `cfg(test)` attribute marks the helper functions as test code for
//! the Clippy settings of `clippy.toml` (unwrap, expect, panic, print).
#![cfg(test)]

use std::future::Future;
use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};
use jiff_sqlx::ToSqlx;
use sqlx::{AssertSqlSafe, Connection, PgConnection};
use telemetry_web_domain::battery::gaps::GapKind;
use telemetry_web_domain::battery::{
    ConnectivityRow, EventKind, EventRow, Field, PerField, RangeKey, RawValue, RowTimes,
    SeriesPair, SignalRow, Snapshot, VehicleCount, Window, derive_gaps, newest_connectivity,
    select_events, series_snapshot, tile_snapshot,
};
use telemetry_web_server::battery::postgres::{
    DatabaseOptions, PostgresSource, READ_TRANSACTION, read_snapshot, read_snapshot_with,
};
use telemetry_web_server::battery::source::{BatterySource, SourceError, SourceRequest};

const PRODUCTION_ENDPOINT: &str = "ep-young-dawn-bgyaj83l";
const ROLES_SQL: &str = include_str!("../../../db/roles.sql");
/// The migrations, in order. A new migration file joins this list.
const MIGRATIONS: [&str; 1] = [include_str!("../../../migrations/0001_raw_tables.sql")];

fn test_url() -> String {
    let url = std::env::var("TEST_DATABASE_URL").expect(
        "TEST_DATABASE_URL is not set. The database tests need the test database (README.md, Verification).",
    );
    assert!(
        !url.to_ascii_lowercase().contains(PRODUCTION_ENDPOINT),
        "TEST_DATABASE_URL names the production endpoint; the tests refuse it"
    );
    url
}

async fn execute(conn: &mut PgConnection, sql: &str) {
    sqlx::raw_sql(AssertSqlSafe(sql.to_owned()))
        .execute(conn)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

async fn admin() -> PgConnection {
    let options = DatabaseOptions::with_schema(&test_url(), "public").unwrap();
    let mut conn = PgConnection::connect_with(options.options()).await.unwrap();
    // The role script waits for an advisory lock that parallel tests share.
    execute(&mut conn, "SET statement_timeout = '5min'").await;
    conn
}

fn new_prefix() -> String {
    let mut bytes = [0_u8; 6];
    getrandom::fill(&mut bytes).unwrap();
    let value = bytes
        .iter()
        .fold(0_u64, |acc, byte| (acc << 8) | u64::from(*byte));
    format!("wtest_{value:012x}")
}

async fn run_roles(conn: &mut PgConnection, prefix: &str) {
    run_roles_with(conn, prefix, "").await;
}

/// Runs the role script; a non-empty `web_password` gives the web role
/// LOGIN and that password.
async fn run_roles_with(conn: &mut PgConnection, prefix: &str, web_password: &str) {
    sqlx::query(
        "SELECT set_config('telemetry.ingest_role', $1 || '_ingest', false),
                set_config('telemetry.migrator_role', $1 || '_migrator', false),
                set_config('telemetry.web_role', $1 || '_web', false),
                set_config('telemetry.schema', $1, false),
                set_config('telemetry.web_password', $2, false)",
    )
    .bind(prefix)
    .bind(web_password)
    .execute(&mut *conn)
    .await
    .unwrap();
    execute(conn, &format!("BEGIN; {ROLES_SQL}; COMMIT")).await;
}

/// The rows of one test.
#[derive(Debug, Clone, Default)]
struct Rows {
    vehicles: usize,
    signals: Vec<Signal>,
    connectivity: Vec<ConnectivityRow>,
    events: Vec<EventRow>,
}

#[derive(Debug, Clone)]
struct Signal {
    field: i32,
    created_at: Timestamp,
    value_type: i16,
    int_value: Option<i64>,
    float_value: Option<f64>,
    text_value: Option<String>,
    bool_value: Option<bool>,
}

impl Signal {
    fn raw(&self) -> SignalRow<'_> {
        SignalRow {
            times: RowTimes {
                created_at: self.created_at,
                received_at: received(self.created_at),
            },
            value: RawValue {
                value_type: self.value_type,
                int_value: self.int_value,
                float_value: self.float_value,
                text_value: self.text_value.as_deref(),
                bool_value: self.bool_value,
            },
        }
    }
}

fn received(created_at: Timestamp) -> Timestamp {
    created_at + SignedDuration::from_secs(3)
}

/// Runs `body` with a migrated schema that holds `rows`, on a connection
/// that has the privileges of the web role. Cleans up in every case.
async fn with_rows<F, Fut>(rows: &Rows, body: F)
where
    F: FnOnce(PgConnection, Rows) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let prefix = new_prefix();
    let mut conn = admin().await;
    // The setup and the body run on their own tasks, so a panic becomes a
    // join error and the cleanup still runs.
    let task_rows = rows.clone();
    let task_prefix = prefix.clone();
    let mut setup_conn = admin().await;
    let outcome = tokio::spawn(async move {
        setup(&mut setup_conn, &task_prefix, &task_rows).await;
        setup_conn.close().await.ok();
    })
    .await;
    let result = match outcome {
        Ok(()) => {
            let mut reader = admin().await;
            execute(
                &mut reader,
                &format!("GRANT {prefix}_web TO CURRENT_USER WITH INHERIT FALSE, SET TRUE"),
            )
            .await;
            execute(&mut reader, &format!("SET ROLE {prefix}_web; SET search_path = {prefix}; SET statement_timeout = '10s'")).await;
            tokio::spawn(body(reader, rows.clone())).await
        }
        Err(error) => Err(error),
    };
    cleanup(&mut conn, &prefix).await;
    conn.close().await.ok();
    if let Err(error) = result {
        std::panic::resume_unwind(error.into_panic());
    }
}

#[test]
fn the_migration_list_holds_every_migration() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../migrations");
    let files = std::fs::read_dir(dir).unwrap().filter(|entry| {
        entry
            .as_ref()
            .unwrap()
            .path()
            .extension()
            .is_some_and(|ext| ext == "sql")
    });
    assert_eq!(files.count(), MIGRATIONS.len());
}

async fn setup(conn: &mut PgConnection, prefix: &str, rows: &Rows) {
    run_roles(conn, prefix).await;
    let migrator = format!("{prefix}_migrator");
    execute(
        conn,
        &format!("GRANT {migrator} TO CURRENT_USER WITH INHERIT FALSE, SET TRUE"),
    )
    .await;
    execute(
        conn,
        &format!("SET ROLE {migrator}; SET search_path = {prefix}"),
    )
    .await;
    // `ingest migrate` creates `_sqlx_migrations` before the first
    // migration, which changes its privileges; a stand-in table does here.
    execute(
        conn,
        "CREATE TABLE _sqlx_migrations (version bigint PRIMARY KEY)",
    )
    .await;
    for migration in MIGRATIONS {
        execute(conn, migration).await;
    }
    execute(conn, "RESET ROLE").await;
    // A second run grants the web role the tables that now exist.
    run_roles(conn, prefix).await;
    execute(
        conn,
        &format!("GRANT {migrator} TO CURRENT_USER WITH INHERIT FALSE, SET TRUE"),
    )
    .await;
    execute(
        conn,
        &format!("SET ROLE {migrator}; SET search_path = {prefix}"),
    )
    .await;
    let started = std::time::Instant::now();
    insert(conn, rows).await;
    eprintln!("insert: {:?}", started.elapsed());
    execute(conn, "RESET ROLE; SET search_path = public").await;
}

async fn insert(conn: &mut PgConnection, rows: &Rows) {
    let first_seen = "2026-01-01T00:00:00Z"
        .parse::<Timestamp>()
        .unwrap()
        .to_sqlx();
    for index in 0..rows.vehicles {
        sqlx::query("INSERT INTO vehicle (vin, first_seen_at) VALUES ($1, $2)")
            .bind(format!("TESTVIN000000000{index}"))
            .bind(first_seen)
            .execute(&mut *conn)
            .await
            .unwrap();
    }
    // One statement for each 10,000 rows: the challenge rows hold more
    // than 80,000 signals.
    for chunk in rows.signals.chunks(10_000) {
        let created: Vec<_> = chunk.iter().map(|s| s.created_at.to_sqlx()).collect();
        let received_at: Vec<_> = chunk
            .iter()
            .map(|s| received(s.created_at).to_sqlx())
            .collect();
        let ints: Vec<Option<i64>> = chunk.iter().map(|s| s.int_value).collect();
        let floats: Vec<Option<f64>> = chunk.iter().map(|s| s.float_value).collect();
        let fields: Vec<i32> = chunk.iter().map(|s| s.field).collect();
        let types: Vec<i16> = chunk.iter().map(|s| s.value_type).collect();
        let bools: Vec<Option<bool>> = chunk.iter().map(|s| s.bool_value).collect();
        let texts: Vec<Option<String>> = chunk.iter().map(|s| s.text_value.clone()).collect();
        sqlx::query(
            "INSERT INTO vehicle_signal (created_at, received_at, int_value, float_value, field,
                vehicle_id, value_type, is_resend, bool_value, text_value)
             SELECT c, r, i, f, fi, 1, t, false, b, x
             FROM unnest($1::timestamptz[], $2::timestamptz[], $3::bigint[], $4::float8[],
                         $5::integer[], $6::smallint[], $7::boolean[], $8::text[])
                  AS u (c, r, i, f, fi, t, b, x)",
        )
        .bind(created)
        .bind(received_at)
        .bind(ints)
        .bind(floats)
        .bind(fields)
        .bind(types)
        .bind(bools)
        .bind(texts)
        .execute(&mut *conn)
        .await
        .unwrap();
    }
    let ids: Vec<uuid::Uuid> = rows
        .connectivity
        .iter()
        .map(|row| uuid::Uuid::from_u128(row.connection_id))
        .collect();
    let created: Vec<_> = rows
        .connectivity
        .iter()
        .map(|row| row.created_at.to_sqlx())
        .collect();
    let received_at: Vec<_> = rows
        .connectivity
        .iter()
        .map(|row| received(row.created_at).to_sqlx())
        .collect();
    let statuses: Vec<i32> = rows.connectivity.iter().map(|row| row.status).collect();
    sqlx::query(
        "INSERT INTO connectivity_event (connection_id, created_at, received_at, status,
            vehicle_id, network_interface)
         SELECT i, c, r, s, 1, 'wifi'
         FROM unnest($1::uuid[], $2::timestamptz[], $3::timestamptz[], $4::integer[]) AS u (i, c, r, s)",
    )
    .bind(ids)
    .bind(created)
    .bind(received_at)
    .bind(statuses)
    .execute(&mut *conn)
    .await
    .unwrap();
    insert_events(conn, &rows.events).await;
}

async fn insert_events(conn: &mut PgConnection, events: &[EventRow]) {
    sqlx::query(
        "INSERT INTO ingest_event (run_id, occurred_at, kind, reason, dropped_count,
            first_received_at, last_received_at)
         SELECT * FROM unnest($1::uuid[], $2::timestamptz[], $3::text[], $4::text[],
                              $5::bigint[], $6::timestamptz[], $7::timestamptz[])",
    )
    .bind(
        events
            .iter()
            .map(|event| uuid::Uuid::from_u128(event.run_id))
            .collect::<Vec<_>>(),
    )
    .bind(
        events
            .iter()
            .map(|event| event.occurred_at.to_sqlx())
            .collect::<Vec<_>>(),
    )
    .bind(
        events
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>(),
    )
    .bind(
        events
            .iter()
            .map(|event| event.reason.clone())
            .collect::<Vec<_>>(),
    )
    .bind(
        events
            .iter()
            .map(|event| event.dropped_count)
            .collect::<Vec<_>>(),
    )
    .bind(
        events
            .iter()
            .map(|event| event.first_received_at.map(ToSqlx::to_sqlx))
            .collect::<Vec<_>>(),
    )
    .bind(
        events
            .iter()
            .map(|event| event.last_received_at.map(ToSqlx::to_sqlx))
            .collect::<Vec<_>>(),
    )
    .execute(&mut *conn)
    .await
    .unwrap();
}

async fn cleanup(conn: &mut PgConnection, prefix: &str) {
    execute(conn, "RESET ROLE").await;
    execute(conn, "SELECT pg_advisory_lock(hashtext('telemetry.roles'))").await;
    // The migrator owns the schema and every table; dropping its objects
    // first removes the privileges of the other roles on them.
    for suffix in ["migrator", "ingest", "web"] {
        let role = format!("{prefix}_{suffix}");
        for sql in [
            format!("GRANT {role} TO CURRENT_USER WITH INHERIT FALSE, SET TRUE"),
            format!("SET ROLE {role}"),
            format!("DROP OWNED BY {role} CASCADE"),
            "RESET ROLE".to_owned(),
            format!(
                "DO $$ BEGIN EXECUTE format('REVOKE ALL ON DATABASE %I FROM {role}', current_database()); END $$"
            ),
            format!("DROP ROLE IF EXISTS {role}"),
        ] {
            if let Err(error) = sqlx::raw_sql(AssertSqlSafe(sql.clone()))
                .execute(&mut *conn)
                .await
            {
                eprintln!("cleanup step failed: {sql}: {error}");
            }
        }
    }
    execute(
        conn,
        "SELECT pg_advisory_unlock(hashtext('telemetry.roles'))",
    )
    .await;
    execute(conn, &format!("DROP SCHEMA IF EXISTS {prefix} CASCADE")).await;
    let left: Vec<String> =
        sqlx::query_scalar("SELECT rolname::text FROM pg_roles WHERE rolname LIKE $1 || '%'")
            .bind(prefix)
            .fetch_all(&mut *conn)
            .await
            .unwrap();
    assert!(left.is_empty(), "cleanup left roles: {left:?}");
}

/// The snapshot of the reference rules for the same rows.
fn expected(rows: &Rows, window: Window) -> Snapshot {
    let vehicles = match rows.vehicles {
        0 => VehicleCount::None,
        1 => VehicleCount::One(1),
        _ => VehicleCount::Many,
    };
    if rows.vehicles != 1 {
        return Snapshot {
            window,
            vehicles,
            tiles: PerField::default(),
            series: SeriesPair::default(),
            events: telemetry_web_domain::battery::EventSnapshot::default(),
            connectivity: None,
        };
    }
    let of = |field: Field| -> Vec<SignalRow<'_>> {
        rows.signals
            .iter()
            .filter(|signal| signal.field == field.number())
            .map(Signal::raw)
            .collect()
    };
    let mut tiles = PerField::default();
    for field in Field::ALL {
        *tiles.get_mut(field) = tile_snapshot(&window, &of(field));
    }
    let events = select_events(&window, &rows.events);
    Snapshot {
        window,
        vehicles,
        tiles,
        series: SeriesPair {
            soc: series_snapshot(&window, &of(Field::Soc)).unwrap(),
            nominal_full_pack_energy: series_snapshot(&window, &of(Field::NominalFullPackEnergy))
                .unwrap(),
        },
        connectivity: newest_connectivity(&rows.connectivity, window.to()),
        events,
    }
}

const NOW: &str = "2026-11-27T12:00:00Z";

fn at(text: &str) -> Timestamp {
    text.parse().unwrap()
}

fn float(field: Field, created_at: Timestamp, value: f64) -> Signal {
    Signal {
        field: field.number(),
        created_at,
        value_type: 5,
        int_value: None,
        float_value: Some(value),
        text_value: None,
        bool_value: None,
    }
}

/// A row of the invalid marker (10) or a boolean (6).
fn other(field: Field, created_at: Timestamp, value_type: i16) -> Signal {
    Signal {
        value_type,
        float_value: None,
        bool_value: Some(true),
        ..float(field, created_at, 0.0)
    }
}

// The run ids of the challenge rows do not follow the start order (R1, R2,
// R3 start in this order; their ids sort as R2, R3, R1).
const R1: u128 = 0x9a1;
const R2: u128 = 0x172;
const R3: u128 = 0x553;

#[expect(
    clippy::too_many_lines,
    reason = "one list of challenge rows reads better than parts in several helpers"
)]
/// Rows with every value form, bucket edges, rows outside the window,
/// gaps, and many short connections.
fn challenge_rows() -> Rows {
    let now = at(NOW);
    let mut signals = Vec::new();
    // Soc every 7 minutes for 400 days, so every range has rows at its
    // start, inside, and at its end.
    for step in 1..(400 * 24 * 60 / 7) {
        let created = now - SignedDuration::from_mins(7 * step) - SignedDuration::from_micros(step);
        let value = 50.0 + f64::from(i32::try_from(step % 50).unwrap()) * 0.5;
        signals.push(float(Field::Soc, created, value));
    }
    // A row exactly at a bucket start and one at the window end.
    signals.push(float(Field::Soc, at("2026-11-27T11:00:00.5Z"), 12.25));
    signals.push(float(Field::Soc, now, 99.0));
    // The other value forms of Soc.
    let forms = [
        (
            at("2026-11-27T10:00:30Z"),
            1,
            None,
            None,
            Some("61.5"),
            None,
        ),
        (
            at("2026-11-27T10:07:30Z"),
            1,
            None,
            None,
            Some("1234567890.1234567890123"),
            None,
        ),
        (
            at("2026-11-27T10:01:30Z"),
            1,
            None,
            None,
            Some("-0012.50"),
            None,
        ),
        (at("2026-11-27T10:02:30Z"), 1, None, None, Some("1e5"), None),
        (at("2026-11-27T10:03:30Z"), 1, None, None, Some("5\n"), None),
        (at("2026-11-27T10:04:30Z"), 1, None, None, Some(" 5"), None),
        (at("2026-11-27T10:05:30Z"), 1, None, None, Some("NaN"), None),
        (
            at("2026-11-27T10:06:30Z"),
            1,
            None,
            None,
            Some("123456789012345678901234567890123"),
            None,
        ),
        (at("2026-11-27T10:08:30Z"), 2, Some(62), None, None, None),
        (
            at("2026-11-27T10:09:30Z"),
            3,
            Some(9_007_199_254_740_993),
            None,
            None,
            None,
        ),
        (
            at("2026-11-27T10:10:30Z"),
            3,
            Some(-9_007_199_254_740_992),
            None,
            None,
            None,
        ),
        (
            at("2026-11-27T10:11:30Z"),
            5,
            None,
            Some(f64::NAN),
            None,
            None,
        ),
        (
            at("2026-11-27T10:12:30Z"),
            5,
            None,
            Some(f64::INFINITY),
            None,
            None,
        ),
        (
            at("2026-11-27T10:13:30Z"),
            4,
            None,
            Some(f64::NEG_INFINITY),
            None,
            None,
        ),
        (at("2026-11-27T10:14:30Z"), 10, None, None, None, Some(true)),
        (at("2026-11-27T10:15:30Z"), 6, None, None, None, Some(false)),
        (at("2026-11-27T10:16:30Z"), 8, Some(3), None, None, None),
    ];
    for (created, value_type, int_value, float_value, text_value, bool_value) in forms {
        signals.push(Signal {
            field: Field::Soc.number(),
            created_at: created,
            value_type,
            int_value,
            float_value,
            text_value: text_value.map(str::to_owned),
            bool_value,
        });
    }
    // The capacity: one value long before every range, a few in the
    // year, the invalid marker as the newest row.
    for (created, value) in [
        (at("2024-01-01T00:00:00Z"), 80.0),
        (at("2026-03-01T00:00:00Z"), 79.2),
        (at("2026-11-01T00:00:00Z"), 78.9),
        (at("2026-11-27T09:00:00Z"), 78.8),
    ] {
        signals.push(float(Field::NominalFullPackEnergy, created, value));
    }
    signals.push(other(
        Field::NominalFullPackEnergy,
        at("2026-11-27T11:30:00Z"),
        10,
    ));
    signals.push(float(
        Field::EnergyRemaining,
        at("2026-11-27T11:40:00Z"),
        41.5,
    ));
    signals.push(other(Field::OutsideTemp, at("2026-11-27T11:41:00Z"), 10));
    // Another field: never read.
    signals.push(float(Field::Soc, at("2026-11-27T11:42:00Z"), 1.0));
    signals.last_mut().unwrap().field = 42;

    let mut connectivity = Vec::new();
    for index in 0..250_u128 {
        let start = at("2026-11-27T08:00:00Z")
            + SignedDuration::from_secs(i64::try_from(index).unwrap() * 50);
        connectivity.push(ConnectivityRow {
            connection_id: index + 1,
            created_at: start,
            status: 1,
        });
        let end = start + SignedDuration::from_secs(20);
        connectivity.push(ConnectivityRow {
            connection_id: index + 1,
            created_at: end,
            status: 2,
        });
    }
    connectivity.push(ConnectivityRow {
        connection_id: 999,
        created_at: at("2026-11-27T11:58:00Z"),
        status: 1,
    });
    // A row at the same instant with a lower connection id, another
    // status, and a row at the window end: the newest fact stays the row
    // 999.
    // One connection with CONNECTED and DISCONNECTED at one instant,
    // before the newest fact: not the fact.
    for status in [1, 2] {
        connectivity.push(ConnectivityRow {
            connection_id: 997,
            created_at: at("2026-11-27T11:57:00Z"),
            status,
        });
    }
    connectivity.push(ConnectivityRow {
        connection_id: 998,
        created_at: at("2026-11-27T11:58:00Z"),
        status: 2,
    });
    connectivity.push(ConnectivityRow {
        connection_id: 1_000,
        created_at: at("2026-11-27T11:59:00Z"),
        status: 0,
    });
    connectivity.push(ConnectivityRow {
        connection_id: 1_001,
        created_at: at(NOW),
        status: 2,
    });

    let event = |run_id: u128, occurred: &str, kind: EventKind, reason: Option<&str>| EventRow {
        run_id,
        occurred_at: at(occurred),
        kind,
        reason: reason.map(str::to_owned),
        dropped_count: None,
        first_received_at: None,
        last_received_at: None,
    };
    let lost =
        |run_id: u128, kind: EventKind, reason: Option<&str>, first: &str, last: &str| EventRow {
            dropped_count: Some(12),
            first_received_at: Some(at(first)),
            last_received_at: Some(at(last)),
            ..event(run_id, last, kind, reason)
        };
    let mut events = vec![
        event(R1, "2025-10-01T08:00:00Z", EventKind::Started, None),
        event(R2, "2026-11-20T08:00:00Z", EventKind::Started, None),
        event(R3, "2026-11-27T07:00:00Z", EventKind::Started, None),
        // Two starts at one instant: the run id orders them.
        event(R1, "2026-11-27T07:00:00Z", EventKind::Started, None),
        // A start at the window end: in no range.
        event(R2, NOW, EventKind::Started, None),
        event(
            R2,
            "2026-11-27T07:00:07Z",
            EventKind::Stopped,
            Some("SIGTERM"),
        ),
        event(
            R3,
            "2026-11-27T09:00:00Z",
            EventKind::GateClosed,
            Some("child_not_running"),
        ),
        event(
            R3,
            "2026-11-27T09:00:00.000001Z",
            EventKind::ChildExited,
            Some("exit code 1"),
        ),
    ];
    events.extend([
        lost(
            R3,
            EventKind::RecordsDropped,
            Some("queue_full"),
            "2026-11-27T10:00:00Z",
            "2026-11-27T10:05:00Z",
        ),
        lost(
            R2,
            EventKind::RecordsDropped,
            Some("database_refused"),
            "2026-11-27T10:00:00Z",
            "2026-11-27T10:05:00Z",
        ),
        lost(
            R3,
            EventKind::ZmqShortfall,
            None,
            "2026-11-27T11:00:00Z",
            "2026-11-27T11:00:30Z",
        ),
        // No recorded loss: refused connections, and a reason that this
        // version does not know.
        lost(
            R3,
            EventKind::GateFull,
            None,
            "2026-11-27T11:10:00Z",
            "2026-11-27T11:10:30Z",
        ),
        lost(
            R3,
            EventKind::RecordsDropped,
            Some("new_reason"),
            "2026-11-27T11:20:00Z",
            "2026-11-27T11:20:30Z",
        ),
        // Starts one microsecond before the window end: in every range;
        // starts at the window end: in none.
        lost(
            R1,
            EventKind::ZmqShortfall,
            None,
            "2026-11-27T11:59:59.999999Z",
            "2026-11-27T11:59:59.999999Z",
        ),
        lost(
            R2,
            EventKind::ZmqShortfall,
            None,
            "2026-11-27T12:00:00Z",
            "2026-11-27T12:00:00Z",
        ),
    ]);
    let mut rows = Rows {
        vehicles: 1,
        signals,
        connectivity,
        events,
    };
    edge_rows(&mut rows);
    rows
}

/// Rows at the edges of the SQL bounds, for each range: signals exactly
/// at the window start and one microsecond before and after it, unusable
/// rows as the nearest candidates of the carry-in and the first value,
/// loss rows whose interval ends at the window start or one microsecond
/// before it, and starts at the window start or one microsecond before it.
fn edge_rows(rows: &mut Rows) {
    let tick = SignedDuration::from_micros(1);
    let minute = SignedDuration::from_mins(1);
    for (index, key) in RangeKey::ALL.into_iter().enumerate() {
        let from = Window::ending_at(key, at(NOW)).unwrap().from();
        let value = 70.0 + f64::from(u8::try_from(index).unwrap());
        for created in [from - tick, from, from + tick] {
            rows.signals.push(float(Field::Soc, created, value));
        }
        // The capacity: an unusable row is the newest row before the
        // start and the first row in the range.
        rows.signals
            .push(float(Field::NominalFullPackEnergy, from - minute, value));
        rows.signals
            .push(other(Field::NominalFullPackEnergy, from - tick, 10));
        rows.signals
            .push(other(Field::NominalFullPackEnergy, from, 10));
        rows.signals.push(float(
            Field::NominalFullPackEnergy,
            from + minute,
            value + 0.5,
        ));
        let run = 0x0e0 + u128::try_from(index).unwrap();
        for last in [from - tick, from] {
            rows.events.push(EventRow {
                run_id: run,
                occurred_at: last + tick,
                kind: EventKind::RecordsDropped,
                reason: Some("queue_full".to_owned()),
                dropped_count: Some(2),
                first_received_at: Some(last - minute),
                last_received_at: Some(last),
            });
            rows.events.push(EventRow {
                run_id: run,
                occurred_at: last,
                kind: EventKind::Started,
                reason: None,
                dropped_count: None,
                first_received_at: None,
                last_received_at: None,
            });
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_sql_equals_the_reference_rules_for_every_range() {
    let rows = challenge_rows();
    with_rows(&rows, async move |mut conn: PgConnection, rows: Rows| {
        for key in RangeKey::ALL {
            let window = Window::ending_at(key, at(NOW)).unwrap();
            let started = std::time::Instant::now();
            let snapshot = read_snapshot(&mut conn, window).await.unwrap();
            eprintln!("read {key:?}: {:?}", started.elapsed());
            assert_eq!(snapshot, expected(&rows, window), "{key:?}");
        }
        // The rows exercise each rule; the values are written out.
        let window = Window::ending_at(RangeKey::Day, at(NOW)).unwrap();
        let snapshot = read_snapshot(&mut conn, window).await.unwrap();
        assert_eq!(snapshot.series.soc.excluded.invalid_marker, 1);
        assert_eq!(snapshot.series.soc.excluded.not_numeric, 11);
        assert_eq!(
            snapshot.connectivity,
            Some(ConnectivityRow {
                connection_id: 999,
                created_at: at("2026-11-27T11:58:00Z"),
                status: 1,
            })
        );
        assert_eq!(
            snapshot.events.restarts.items,
            [
                window.from(),
                at("2026-11-27T07:00:00Z"),
                at("2026-11-27T07:00:00Z")
            ]
        );
        let gaps = derive_gaps(&snapshot.events.losses).unwrap();
        assert!(!gaps.truncated);
        assert_eq!(
            gaps.items
                .iter()
                .map(|gap| (gap.kind, gap.from, gap.to))
                .collect::<Vec<_>>(),
            [
                (
                    GapKind::QueueFull,
                    window.from() - SignedDuration::from_mins(1),
                    window.from()
                ),
                (
                    GapKind::DatabaseRefused,
                    at("2026-11-27T10:00:00Z"),
                    at("2026-11-27T10:05:00Z")
                ),
                (
                    GapKind::QueueFull,
                    at("2026-11-27T10:00:00Z"),
                    at("2026-11-27T10:05:00Z")
                ),
                (
                    GapKind::ZmqShortfall,
                    at("2026-11-27T11:00:00Z"),
                    at("2026-11-27T11:00:30Z")
                ),
                (
                    GapKind::ZmqShortfall,
                    at("2026-11-27T11:59:59.999999Z"),
                    at("2026-11-27T11:59:59.999999Z")
                ),
            ]
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn no_vehicle_and_two_vehicles_read_as_the_reference_rules() {
    for vehicles in [0, 2] {
        let rows = Rows {
            vehicles,
            ..Rows::default()
        };
        with_rows(&rows, async move |mut conn: PgConnection, rows: Rows| {
            let window = Window::ending_at(RangeKey::Week, at(NOW)).unwrap();
            assert_eq!(
                read_snapshot(&mut conn, window).await.unwrap(),
                expected(&rows, window)
            );
        })
        .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_vehicle_reads_as_the_reference_rules() {
    let rows = Rows {
        vehicles: 1,
        ..Rows::default()
    };
    with_rows(&rows, async move |mut conn: PgConnection, rows: Rows| {
        let window = Window::ending_at(RangeKey::Year, at(NOW)).unwrap();
        assert_eq!(
            read_snapshot(&mut conn, window).await.unwrap(),
            expected(&rows, window)
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_read_transaction_is_one_read_only_snapshot() {
    let rows = Rows {
        vehicles: 1,
        signals: vec![float(Field::Soc, at("2026-11-27T10:00:00Z"), 50.0)],
        ..Rows::default()
    };
    with_rows(&rows, async move |mut conn: PgConnection, rows: Rows| {
        // The transaction of a read: repeatable read, read only. A write in
        // it fails with 25006 before the privilege check.
        execute(&mut conn, READ_TRANSACTION).await;
        let settings: (String, String) = sqlx::query_as(
            "SELECT current_setting('transaction_isolation'), current_setting('transaction_read_only')",
        )
        .fetch_one(&mut conn)
        .await
        .unwrap();
        assert_eq!(settings, ("repeatable read".to_owned(), "on".to_owned()));
        let error = sqlx::query("DELETE FROM ingest_event")
            .execute(&mut conn)
            .await
            .unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("25006")
        );
        execute(&mut conn, "ROLLBACK").await;

        // A write that commits after the first statement of the read is
        // not in the snapshot of the later statements.
        let schema: String = sqlx::query_scalar("SELECT current_setting('search_path')")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        let window = Window::ending_at(RangeKey::Day, at(NOW)).unwrap();
        let snapshot = read_snapshot_with(&mut conn, window, async || {
            let mut writer = admin().await;
            execute(
                &mut writer,
                &format!(
                    "SET ROLE {schema}_migrator; SET search_path = {schema};
                     INSERT INTO vehicle_signal (created_at, received_at, float_value, field,
                         vehicle_id, value_type, is_resend)
                     VALUES ('2026-11-27T11:00:00Z', '2026-11-27T11:00:03Z', 60, 8, 1, 5, false);
                     INSERT INTO connectivity_event (connection_id, created_at, received_at,
                         status, vehicle_id, network_interface)
                     VALUES (gen_random_uuid(), '2026-11-27T11:00:00Z', '2026-11-27T11:00:01Z',
                         1, 1, 'wifi')"
                ),
            )
            .await;
            writer.close().await.ok();
        })
        .await
        .unwrap();
        assert_eq!(snapshot, expected(&rows, window));
        // A new read sees the write.
        assert_ne!(read_snapshot(&mut conn, window).await.unwrap(), snapshot);
    })
    .await;
}

/// The URL of `TEST_DATABASE_URL` with another user and password.
fn url_for(user: &str, password: &str) -> String {
    let url = test_url();
    let (scheme, rest) = url.split_once("://").unwrap();
    let host = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
    format!("{scheme}://{user}:{password}@{host}")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_source_reads_as_the_web_role_over_its_own_login() {
    let rows = Rows {
        vehicles: 1,
        signals: vec![float(Field::Soc, at("2026-11-27T10:00:00Z"), 50.0)],
        // One connection with CONNECTED and DISCONNECTED at one instant:
        // the fact is DISCONNECTED (the status orders them).
        connectivity: [1, 2]
            .into_iter()
            .map(|status| ConnectivityRow {
                connection_id: 7,
                created_at: at("2026-11-27T10:00:00Z"),
                status,
            })
            .collect(),
        ..Rows::default()
    };
    with_rows(&rows, async move |mut conn: PgConnection, rows: Rows| {
        let schema: String = sqlx::query_scalar("SELECT current_setting('search_path')")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes).unwrap();
        let password = bytes
            .iter()
            .fold(String::new(), |text, byte| text + &format!("{byte:02x}"));
        let mut owner = admin().await;
        run_roles_with(&mut owner, &schema, &password).await;
        owner.close().await.ok();
        let options =
            DatabaseOptions::with_schema(&url_for(&format!("{schema}_web"), &password), &schema)
                .unwrap();
        let source = PostgresSource::with_clock(options, Arc::new(|| at(NOW)));
        let request = SourceRequest {
            key: RangeKey::Day,
            scenario: None,
        };
        let window = Window::ending_at(RangeKey::Day, at(NOW)).unwrap();
        let snapshot = source.snapshot(&request).await.unwrap();
        assert_eq!(snapshot, expected(&rows, window));
        assert_eq!(
            snapshot.connectivity,
            Some(ConnectivityRow {
                connection_id: 7,
                created_at: at("2026-11-27T10:00:00Z"),
                status: 2,
            })
        );
    })
    .await;
}

/// The limits: 502 losses and 501 starts in the range give 500 of each
/// and the mark that the limit cut the list; the cut keeps the oldest.
/// The losses 499 to 502 share their times. The SQL reads 501 rows, so
/// its order by run id decides which rows reach the cut, and the order of
/// the reference rules (run id, then kind) decides items 499 and 500. A
/// tie of kinds within one run cannot change the result of the SQL cut:
/// both rows of that run are among the 501 rows.
#[tokio::test(flavor = "multi_thread")]
async fn a_limit_cuts_the_lists_and_says_so() {
    let start = at("2026-11-27T00:00:00Z");
    let loss = |run_id: u128, kind: EventKind, time: Timestamp| EventRow {
        run_id,
        occurred_at: time,
        kind,
        reason: (kind == EventKind::RecordsDropped).then(|| "queue_full".to_owned()),
        dropped_count: Some(1),
        first_received_at: Some(time),
        last_received_at: Some(time),
    };
    let mut events = Vec::new();
    for index in 0..=500_i64 {
        let time = start + SignedDuration::from_secs(index);
        if index < 498 {
            events.push(loss(7, EventKind::ZmqShortfall, time));
        }
        events.push(EventRow {
            run_id: 8,
            occurred_at: time,
            kind: EventKind::Started,
            reason: None,
            dropped_count: None,
            first_received_at: None,
            last_received_at: None,
        });
    }
    let tie = start + SignedDuration::from_secs(498);
    events.extend([
        loss(0x30, EventKind::ZmqShortfall, tie),
        loss(0x20, EventKind::ZmqShortfall, tie),
        loss(0x10, EventKind::ZmqShortfall, tie),
        loss(0x10, EventKind::RecordsDropped, tie),
    ]);
    let rows = Rows {
        vehicles: 1,
        signals: vec![float(Field::Soc, at("2026-11-27T10:00:00Z"), 50.0)],
        events,
        ..Rows::default()
    };
    with_rows(&rows, async move |mut conn: PgConnection, rows: Rows| {
        let window = Window::ending_at(RangeKey::Day, at(NOW)).unwrap();
        let snapshot = read_snapshot(&mut conn, window).await.unwrap();
        assert_eq!(snapshot, expected(&rows, window));
        let losses = &snapshot.events.losses;
        assert_eq!(losses.items.len(), 500);
        assert!(losses.truncated);
        assert_eq!(losses.items[0].first_received_at, Some(start));
        // Items 499 and 500: run 0x10 `records_dropped`, then run 0x10
        // `zmq_shortfall`; runs 0x20 and 0x30 are cut.
        let tail: Vec<(u128, EventKind)> = losses.items[498..]
            .iter()
            .map(|row| (row.run_id, row.kind))
            .collect();
        assert_eq!(
            tail,
            [
                (0x10, EventKind::RecordsDropped),
                (0x10, EventKind::ZmqShortfall)
            ]
        );
        assert_eq!(snapshot.events.restarts.items.len(), 500);
        assert!(snapshot.events.restarts.truncated);
        assert_eq!(snapshot.events.restarts.items[0], start);
        // At the limit, no cut: a window that starts one second after the
        // first start holds 500 starts, and one that starts two seconds
        // after the first loss holds 500 losses.
        let later = |seconds| {
            Window::ending_at(
                RangeKey::Day,
                start + SignedDuration::from_hours(24) + SignedDuration::from_secs(seconds),
            )
            .unwrap()
        };
        let snapshot = read_snapshot(&mut conn, later(1)).await.unwrap();
        assert_eq!(snapshot, expected(&rows, later(1)));
        assert_eq!(snapshot.events.restarts.items.len(), 500);
        assert!(!snapshot.events.restarts.truncated);
        let snapshot = read_snapshot(&mut conn, later(2)).await.unwrap();
        assert_eq!(snapshot, expected(&rows, later(2)));
        assert_eq!(snapshot.events.losses.items.len(), 500);
        assert!(!snapshot.events.losses.truncated);
    })
    .await;
}

/// Removes the schemas and roles of earlier runs that a failed cleanup
/// left. Run it on request only: `TEST_SWEEP=1`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "manual cleanup of the test branch"]
async fn sweep_left_prefixes() {
    let mut conn = admin().await;
    let prefixes: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT substring(rolname from '^(wtest_[0-9a-f]{12})_') FROM pg_roles WHERE rolname LIKE 'wtest\\_%'",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    for prefix in prefixes {
        cleanup(&mut conn, &prefix).await;
    }
}

#[tokio::test]
async fn an_unreachable_database_is_unavailable_not_empty() {
    // Port 1 on the loopback interface refuses the connection at once.
    let options =
        DatabaseOptions::from_url("postgresql://telemetry_web:x@127.0.0.1:1/neondb").unwrap();
    let source = PostgresSource::with_clock(options, Arc::new(|| at(NOW)));
    let request = SourceRequest {
        key: RangeKey::Day,
        scenario: None,
    };
    let error = source.snapshot(&request).await.unwrap_err();
    assert!(matches!(error, SourceError::Unavailable(_)), "{error:?}");
}

/// A Gear, Odometer, `EnergyRemaining`, `OutsideTemp`, or `VehicleSpeed`
/// row of the consumption tests.
fn row(field: i32, created_at: &str, value_type: i16, value: f64) -> Signal {
    Signal {
        field,
        created_at: at(created_at),
        value_type,
        int_value: None,
        float_value: None,
        text_value: None,
        bool_value: None,
    }
    .with_value(value)
}

impl Signal {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the test passes whole numbers for the integer types"
    )]
    fn with_value(mut self, value: f64) -> Self {
        match self.value_type {
            4 | 5 => self.float_value = Some(value),
            1 => self.text_value = Some(value.to_string()),
            6 | 10 => self.bool_value = Some(true),
            _ => self.int_value = Some(value as i64),
        }
        self
    }
}

/// The SQL of the consumption view selects the rows of the rules of
/// `telemetry_web_domain::consumption`, as the web role, on boundary rows:
/// a drive across the range start, invalid Gear readings, samples exactly
/// at the drive ends and just after, a text value, a value row of another
/// field, and an ongoing drive at the range end.
#[tokio::test(flavor = "multi_thread")]
async fn the_consumption_read_selects_the_rows_of_the_drive_rules() {
    use telemetry_web_domain::consumption::derive_view;
    use telemetry_web_server::consumption::postgres::read_consumption;
    // The window of the week that ends at NOW starts at 2026-11-20T12:00Z.
    let rows = Rows {
        vehicles: 1,
        signals: vec![
            // The newest reading before the range is D: the drive that
            // ends at 12:10 in the range is left out.
            row(10, "2026-11-20T11:50:00Z", 9, 5.0),
            row(10, "2026-11-20T12:10:00Z", 9, 2.0),
            row(5, "2026-11-20T12:05:00Z", 5, 1.0),
            // Drive 1: D at 08:00, invalid readings, P at 08:30.
            row(10, "2026-11-25T08:00:00Z", 9, 5.0),
            row(10, "2026-11-25T08:10:00Z", 10, 0.0),
            row(10, "2026-11-25T08:11:00Z", 9, 1.0),
            row(10, "2026-11-25T08:12:00Z", 9, 6.0),
            row(10, "2026-11-25T08:30:00Z", 9, 2.0),
            // Values at the exact ends count; one after the end does not.
            row(5, "2026-11-25T08:00:00Z", 5, 100.0),
            row(5, "2026-11-25T08:30:00Z", 1, 112.5),
            row(5, "2026-11-25T08:30:00.000001Z", 5, 999.0),
            // The lookback: the newest sample within one hour before the
            // start is the start value; an older one is not read.
            row(158, "2026-11-25T07:50:00Z", 5, 61.0),
            row(5, "2026-11-25T06:59:59Z", 5, 50.0),
            row(158, "2026-11-25T08:00:30Z", 5, 60.0),
            row(158, "2026-11-25T08:15:00Z", 10, 0.0),
            row(158, "2026-11-25T08:30:00Z", 5, 56.0),
            row(86, "2026-11-25T08:05:00Z", 5, -3.0),
            row(86, "2026-11-25T08:10:00Z", 5, -1.0),
            // A field that the view does not read.
            row(4, "2026-11-25T08:20:00Z", 5, 80.0),
            // An ongoing drive at the range end.
            row(10, "2026-11-27T11:55:00Z", 9, 4.0),
        ],
        ..Rows::default()
    };
    with_rows(&rows, async move |mut conn: PgConnection, _rows: Rows| {
        let window = Window::ending_at(RangeKey::Week, at(NOW)).unwrap();
        let snapshot = read_consumption(&mut conn, window).await.unwrap();
        assert!(!snapshot.gears.truncated);
        assert_eq!(snapshot.gears.readings.len(), 4);
        assert_eq!(snapshot.samples.samples.len(), 7);
        let view = derive_view(&snapshot).unwrap();
        let starts: Vec<_> = view.drives.iter().map(|d| d.start).collect();
        assert_eq!(
            starts,
            [at("2026-11-27T11:55:00Z"), at("2026-11-25T08:00:00Z")]
        );
        let drive = view.drives[1];
        assert_eq!(drive.end, Some(at("2026-11-25T08:30:00Z")));
        assert_eq!(drive.distance_mi, Some(12.5));
        assert_eq!(drive.energy_kwh, Some(5.0));
        assert_eq!(drive.mean_outside_temp_c, Some(-2.0));
        assert_eq!(view.drives[0].end, None);
        assert_eq!(view.totals.drives, 1);
    })
    .await;
}

/// A recorded loss that meets a drive marks it, and the response has the
/// status of the battery view, from the same statements.
#[tokio::test(flavor = "multi_thread")]
async fn the_consumption_read_marks_a_drive_with_a_recorded_loss() {
    use telemetry_web_domain::battery::Freshness;
    use telemetry_web_domain::consumption::derive_view;
    use telemetry_web_server::consumption::postgres::read_consumption;
    let rows = Rows {
        vehicles: 1,
        signals: vec![
            row(10, "2026-11-27T08:00:00Z", 9, 5.0),
            row(10, "2026-11-27T08:30:00Z", 9, 2.0),
            row(5, "2026-11-27T08:00:00Z", 5, 100.0),
            row(5, "2026-11-27T08:30:00Z", 5, 110.0),
            row(158, "2026-11-27T08:00:00Z", 5, 60.0),
            row(158, "2026-11-27T08:30:00Z", 5, 57.0),
        ],
        connectivity: vec![ConnectivityRow {
            connection_id: 0x77,
            created_at: at("2026-11-27T07:59:00Z"),
            status: 2,
        }],
        events: vec![EventRow {
            run_id: 0x91,
            occurred_at: at("2026-11-27T08:20:00Z"),
            kind: EventKind::RecordsDropped,
            reason: Some("queue_full".to_owned()),
            dropped_count: Some(4),
            first_received_at: Some(at("2026-11-27T08:10:00Z")),
            last_received_at: Some(at("2026-11-27T08:11:00Z")),
        }],
    };
    with_rows(&rows, async move |mut conn: PgConnection, _rows: Rows| {
        let window = Window::ending_at(RangeKey::Day, at(NOW)).unwrap();
        let view = derive_view(&read_consumption(&mut conn, window).await.unwrap()).unwrap();
        assert_eq!(view.gaps.items.len(), 1);
        assert!(view.drives[0].data_loss);
        assert_eq!(view.drives[0].distance_mi, Some(10.0));
        assert_eq!(view.totals.with_data_loss, 1);
        let status = view.status.unwrap();
        assert_eq!(status.latest_data_at, at("2026-11-27T08:30:00Z"));
        assert_eq!(status.freshness, Freshness::NotExpected);
    })
    .await;
}

/// The production state today: one vehicle, a P reading and an invalid
/// Gear reading, and no drive. The second statement gets empty arrays.
#[tokio::test(flavor = "multi_thread")]
async fn the_consumption_read_without_a_drive_gives_no_drive() {
    use telemetry_web_domain::consumption::derive_view;
    use telemetry_web_server::consumption::postgres::read_consumption;
    let rows = Rows {
        vehicles: 1,
        signals: vec![
            row(10, "2026-11-27T08:00:00Z", 9, 2.0),
            row(10, "2026-11-27T09:00:00Z", 10, 0.0),
            row(5, "2026-11-27T08:00:00Z", 5, 1000.0),
        ],
        ..Rows::default()
    };
    with_rows(&rows, async move |mut conn: PgConnection, _rows: Rows| {
        let window = Window::ending_at(RangeKey::Day, at(NOW)).unwrap();
        let snapshot = read_consumption(&mut conn, window).await.unwrap();
        assert_eq!(snapshot.gears.readings.len(), 1);
        assert_eq!(snapshot.samples.samples, Vec::new());
        let view = derive_view(&snapshot).unwrap();
        assert_eq!(view.drives, Vec::new());
        assert_eq!(view.totals.drives, 0);
    })
    .await;
}

/// No vehicle reads as no drive; two vehicles are a contract violation.
#[tokio::test(flavor = "multi_thread")]
async fn the_consumption_read_of_no_vehicle_and_two_vehicles() {
    use telemetry_web_domain::consumption::derive_view;
    use telemetry_web_server::consumption::postgres::read_consumption;
    for (vehicles, drives) in [(0, Some(0)), (2, None)] {
        let rows = Rows {
            vehicles,
            ..Rows::default()
        };
        with_rows(&rows, async move |mut conn: PgConnection, _rows: Rows| {
            let window = Window::ending_at(RangeKey::Day, at(NOW)).unwrap();
            let snapshot = read_consumption(&mut conn, window).await.unwrap();
            assert_eq!(
                derive_view(&snapshot).ok().map(|view| view.drives.len()),
                drives
            );
        })
        .await;
    }
}
