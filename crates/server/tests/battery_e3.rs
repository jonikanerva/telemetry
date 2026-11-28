//! Experiment E3: the cost of the battery read on Neon (ADR 0013).
//!
//! Run on request only, against the Neon branch `test`:
//!
//! ```sh
//! set -a; . ./.env.test; set +a
//! cargo nextest run -p telemetry-web-server --locked --test battery_e3 \
//!     --run-ignored ignored-only --no-capture
//! ```
//!
//! The run loads 90 days of synthetic rows at the planning rate of
//! `docs/telemetry-usage-estimate.md` (about 4,500 signal rows a day of
//! all fields, 135,000 a month) into a schema of its own, reads each range
//! several times, prints the client time of each read and the plan of the
//! two largest statements, and drops the schema. `E3_KEEP=1` keeps the
//! schema and prints its name; `E3_SCHEMA=<name>` reads an existing
//! schema without a load (a cold read after the compute suspends), and
//! `E3_DROP=1` with it drops that schema.
//!
//! The read runs as the owner role with the `search_path` of the schema:
//! the experiment measures the statements, not the privileges.

//!
//! The `cfg(test)` attribute marks the helper functions as test code for
//! the Clippy settings of `clippy.toml` (unwrap, expect, panic, print).
#![cfg(test)]

use std::time::{Duration, Instant};

use jiff::Timestamp;
use sqlx::{AssertSqlSafe, Connection, PgConnection};
use telemetry_web_domain::battery::{RangeKey, Window};
use telemetry_web_server::battery::postgres::{DatabaseOptions, read_snapshot};

const MIGRATION: &str = include_str!("../../../migrations/0001_raw_tables.sql");
const DAYS: i64 = 90;

/// Fields and their rows on one day: the monthly planning hours of each
/// state times the hourly rate of each field, divided by 30.
const DAILY_ROWS: [(i32, i64); 17] = [
    (8, 266),   // Soc
    (42, 266),  // BatteryLevel
    (158, 532), // EnergyRemaining
    (263, 4),   // NominalFullPackEnergyKwh
    (86, 53),   // OutsideTemp
    (85, 53),   // InsideTemp
    (4, 1_800), // VehicleSpeed (driving 1 h a day)
    (9, 720),   // Location
    (5, 120),   // Odometer
    (10, 10),   // Gear
    (2, 3),     // DetailedChargeState
    (21, 213),  // ACChargingPower
    (22, 107),  // ACChargingEnergyIn
    (24, 4),    // DCChargingEnergyIn
    (25, 36),   // DCChargingPower
    (70, 106),  // TpmsPressureFl (one field stands for the four)
    (71, 318),  // the other three tyre pressures
];

fn url() -> String {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is not set");
    assert!(!url.to_ascii_lowercase().contains("ep-young-dawn-bgyaj83l"));
    url
}

async fn connect(schema: &str) -> (PgConnection, Duration) {
    let options = DatabaseOptions::with_schema(&url(), schema).unwrap();
    let started = Instant::now();
    let conn = PgConnection::connect_with(options.options()).await.unwrap();
    (conn, started.elapsed())
}

async fn execute(conn: &mut PgConnection, sql: &str) {
    sqlx::raw_sql(AssertSqlSafe(sql.to_owned()))
        .execute(conn)
        .await
        .unwrap_or_else(|error| panic!("{error}"));
}

async fn load(schema: &str, now: Timestamp) {
    let (mut conn, _) = connect("public").await;
    execute(&mut conn, "SET statement_timeout = '10min'").await;
    execute(
        &mut conn,
        &format!("CREATE SCHEMA {schema}; SET search_path = {schema}"),
    )
    .await;
    execute(
        &mut conn,
        "CREATE TABLE _sqlx_migrations (version bigint PRIMARY KEY)",
    )
    .await;
    execute(&mut conn, MIGRATION).await;
    execute(
        &mut conn,
        "INSERT INTO vehicle (vin, first_seen_at) VALUES ('TESTVIN0000000001', now())",
    )
    .await;
    let started = Instant::now();
    for (field, per_day) in DAILY_ROWS {
        // The car is awake from 07:00 for 4.4 hours each day; the rows of a
        // field are evenly spread over that time.
        sqlx::query(
            "INSERT INTO vehicle_signal (created_at, received_at, float_value, field, vehicle_id,
                value_type, is_resend)
             SELECT t, t + interval '2 s', 20 + 60 * random(), $1, 1, 5, false
             FROM generate_series(1, $2::integer) AS d,
                  generate_series(0, $3::integer - 1) AS i,
                  LATERAL (SELECT date_trunc('day', $4::timestamptz) - d * interval '1 day'
                      + interval '7 hours' + i * (interval '264 minutes' / $3::integer) AS t) AS x",
        )
        .bind(field)
        .bind(i32::try_from(DAYS).unwrap())
        .bind(i32::try_from(per_day).unwrap())
        .bind(jiff_sqlx::ToSqlx::to_sqlx(now))
        .execute(&mut conn)
        .await
        .unwrap();
    }
    // About 20 connectivity events and 3 ingest events a day.
    execute(
        &mut conn,
        &format!(
            "INSERT INTO connectivity_event (connection_id, created_at, received_at, status, vehicle_id, network_interface)
             SELECT gen_random_uuid(), now() - d * interval '1 hour' * 1.2, now() - d * interval '1 hour' * 1.2, 1 + d % 2, 1, 'wifi'
             FROM generate_series(1, {}) AS d;
             INSERT INTO ingest_event (run_id, occurred_at, kind)
             SELECT gen_random_uuid(), now() - d * interval '8 hours', 'started'
             FROM generate_series(1, {}) AS d;
             ANALYZE vehicle_signal; ANALYZE connectivity_event; ANALYZE ingest_event;",
            DAYS * 20,
            DAYS * 3
        ),
    )
    .await;
    let size: String = sqlx::query_scalar(
        "SELECT pg_size_pretty(pg_total_relation_size('vehicle_signal')) || ' / ' || count(*)::text FROM vehicle_signal",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    eprintln!(
        "E3 load: {:?}, vehicle_signal size and rows: {size}",
        started.elapsed()
    );
    conn.close().await.ok();
}

async fn measure(schema: &str, now: Timestamp, repeats: usize) {
    // The round trip from this client: a statement that does no work.
    let (mut conn, _) = connect(schema).await;
    let mut trips = Vec::new();
    for _ in 0..5 {
        let started = Instant::now();
        sqlx::query("SELECT 1").execute(&mut conn).await.unwrap();
        trips.push(started.elapsed());
    }
    eprintln!("E3 round trip of SELECT 1: {trips:?}");
    // On one connection the statements are prepared after the first read,
    // so a later read costs one round trip for each of its 7 statements
    // plus the server time.
    for key in [RangeKey::Day, RangeKey::Quarter, RangeKey::Year] {
        let window = Window::ending_at(key, now).unwrap();
        let mut reads = Vec::new();
        for _ in 0..3 {
            let started = Instant::now();
            read_snapshot(&mut conn, window).await.unwrap();
            reads.push(started.elapsed());
        }
        eprintln!("E3 reads on one connection {key:?}: {reads:?}");
    }
    let started = Instant::now();
    for _ in 0..7 {
        sqlx::query("SELECT 1").execute(&mut conn).await.unwrap();
    }
    eprintln!("E3 7 round trips: {:?}", started.elapsed());
    conn.close().await.ok();
    for key in [
        RangeKey::Day,
        RangeKey::Week,
        RangeKey::Month,
        RangeKey::Quarter,
        RangeKey::Year,
    ] {
        let window = Window::ending_at(key, now).unwrap();
        let mut times = Vec::new();
        for _ in 0..repeats {
            let (mut conn, connect) = connect(schema).await;
            let started = Instant::now();
            let snapshot = read_snapshot(&mut conn, window).await.unwrap();
            times.push((connect, started.elapsed()));
            let points: usize = snapshot.series.soc.points.len();
            conn.close().await.ok();
            assert!(points > 0 || key == RangeKey::Day);
        }
        eprintln!("E3 read {key:?} (connect, read): {times:?}");
    }
    // The plan of the bucket query of the 90-day range, on the server.
    let (mut conn, _) = connect(schema).await;
    let window = Window::ending_at(RangeKey::Quarter, now).unwrap();
    let plan: Vec<String> = sqlx::query_scalar(
        "EXPLAIN (ANALYZE, BUFFERS, SUMMARY)
        SELECT x.field, max(x.created_at) FILTER (WHERE x.v IS NOT NULL),
            (array_agg(x.v ORDER BY x.created_at DESC) FILTER (WHERE x.v IS NOT NULL))[1],
            min(x.v), max(x.v), count(x.v), count(*) FILTER (WHERE x.value_type = 10), count(*)
        FROM (
            SELECT s.field, s.created_at, s.value_type, CASE
                WHEN s.value_type IN (4, 5) AND s.float_value NOT IN ('NaN', 'Infinity', '-Infinity') THEN s.float_value
                WHEN s.value_type IN (2, 3) AND s.int_value BETWEEN -9007199254740992 AND 9007199254740992 THEN s.int_value::double precision
                WHEN s.value_type = 1 AND length(s.text_value) <= 32 AND s.text_value ~ '^-?[0-9]+(\\.[0-9]+)?$' THEN s.text_value::double precision
            END AS v
            FROM vehicle_signal AS s
            WHERE s.vehicle_id = 1 AND s.field = ANY (ARRAY[8, 263])
              AND s.created_at >= $1 AND s.created_at < $2
        ) AS x
        GROUP BY x.field, date_bin(make_interval(secs => 10800), x.created_at, TIMESTAMPTZ '2000-01-01 00:00:00+00')
        ORDER BY 1",
    )
    .bind(jiff_sqlx::ToSqlx::to_sqlx(window.from()))
    .bind(jiff_sqlx::ToSqlx::to_sqlx(window.to()))
    .fetch_all(&mut conn)
    .await
    .unwrap();
    eprintln!("E3 plan of the 90d bucket query:\n{}", plan.join("\n"));
    conn.close().await.ok();
}

async fn drop_schema(schema: &str) {
    let (mut conn, _) = connect("public").await;
    execute(&mut conn, &format!("DROP SCHEMA {schema} CASCADE")).await;
    conn.close().await.ok();
    eprintln!("E3 dropped {schema}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "experiment E3: run on request against the Neon branch test"]
async fn e3_measure_the_battery_read() {
    let now = Timestamp::now();
    if let Ok(schema) = std::env::var("E3_SCHEMA") {
        measure(&schema, now, 1).await;
        if std::env::var("E3_DROP").is_ok() {
            drop_schema(&schema).await;
        }
        return;
    }
    let mut bytes = [0_u8; 4];
    getrandom::fill(&mut bytes).unwrap();
    let schema = format!("e3_{:08x}", u32::from_le_bytes(bytes));
    load(&schema, now).await;
    measure(&schema, now, 5).await;
    if std::env::var("E3_KEEP").is_ok() {
        eprintln!("E3 kept {schema}");
    } else {
        drop_schema(&schema).await;
    }
}
