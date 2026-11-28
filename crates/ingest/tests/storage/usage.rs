//! The usage measurement of `scripts/usage-measurement.sql`
//! (`docs/telemetry-usage-estimate.md`, "Measure the usage").
//!
//! The test reads the statements from the file, so the file stays the
//! single source. It runs each statement on the migrated schema in a
//! read-only transaction, first on empty tables, then on invented rows,
//! and compares the rows with hand-written expected values. The session
//! time zone is not UTC, so a day that follows the session time zone
//! fails the test.

use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, Connection, PgConnection};

use ingest::storage::migrate;

use crate::support::{TestRoles, VIN, execute, with_migrated, with_roles};

const FILE: &str = include_str!("../../../../scripts/usage-measurement.sql");

/// The statements of the file with the schema of the test in place of
/// `raw`.
fn statements(schema: &str) -> Vec<String> {
    FILE.split(';')
        .filter(|part| part.lines().any(|line| !is_comment(line)))
        .map(|part| {
            assert!(part.contains("raw."), "a statement without raw.: {part}");
            part.replace("raw.", &format!("{schema}."))
        })
        .collect()
}

fn is_comment(line: &str) -> bool {
    let line = line.trim();
    line.is_empty() || line.starts_with("--")
}

/// The rows of `statement` as JSON objects, in the order of the statement.
async fn rows(conn: &mut PgConnection, statement: &str) -> Vec<Value> {
    let mut transaction = conn.begin().await.unwrap();
    execute(&mut transaction, "SET TRANSACTION READ ONLY").await;
    let sql = format!("SELECT to_jsonb(q)::text FROM (\n{statement}\n) AS q");
    let rows: Vec<String> = sqlx::query_scalar(AssertSqlSafe(sql))
        .fetch_all(&mut *transaction)
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
    transaction.rollback().await.unwrap();
    rows.iter()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect()
}

/// Compares `actual` with `expected`. The expected string `"*"` accepts
/// any value except null: the byte sizes depend on the server.
fn matches(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Null, _) => expected.is_null(),
        (_, Value::String(any)) if any == "*" => true,
        (Value::Number(a), Value::Number(e)) => a.as_f64() == e.as_f64(),
        (Value::Array(a), Value::Array(e)) => {
            a.len() == e.len() && a.iter().zip(e).all(|(a, e)| matches(a, e))
        }
        (Value::Object(a), Value::Object(e)) => {
            a.len() == e.len()
                && e.iter()
                    .all(|(key, e)| a.get(key).is_some_and(|a| matches(a, e)))
        }
        _ => actual == expected,
    }
}

async fn check(conn: &mut PgConnection, schema: &str, expected: &[Value]) {
    let statements = statements(schema);
    assert_eq!(statements.len(), expected.len(), "one expectation each");
    for (number, (statement, expected)) in statements.iter().zip(expected).enumerate() {
        let actual = Value::Array(rows(conn, statement).await);
        assert!(
            matches(&actual, expected),
            "statement {}: {actual} is not {expected}",
            number + 1
        );
    }
}

/// The rows of statement `number` (from 1) must match `expected`.
async fn check_one(conn: &mut PgConnection, schema: &str, number: usize, expected: &Value) {
    let actual = Value::Array(rows(conn, &statements(schema)[number - 1]).await);
    assert!(
        matches(&actual, expected),
        "statement {number}: {actual} is not {expected}"
    );
}

/// The file names no column that holds an identity, a place, or a value,
/// and no column of the raw frames.
#[test]
fn the_statements_read_no_private_column() {
    let forbidden = [
        "vin",
        "vehicle",
        "latitude",
        "longitude",
        "int_value",
        "float_value",
        "bool_value",
        "text_value",
        "json_value",
        "payload",
        "topic",
        "body",
        "tags",
        "network_interface",
    ];
    for line in FILE.lines().filter(|line| !is_comment(line)) {
        for token in line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            assert!(
                !forbidden.contains(&token.to_ascii_lowercase().as_str()),
                "{token} in: {line}"
            );
        }
    }
}

/// A day of November 2026 (1 to 9) in UTC, `HH:MM:SS`.
fn nov(day: u8, time: &str) -> String {
    format!("'2026-11-0{day}T{time}Z'")
}

async fn insert_fixtures(conn: &mut PgConnection) {
    execute(
        conn,
        &format!(
            "INSERT INTO vehicle (vin, first_seen_at) VALUES ('{VIN}', {})",
            nov(1, "00:00:00")
        ),
    )
    .await;
    // (day, time, field, value_type, is_resend, value column, value)
    let signals = [
        (1, "10:00:00", 8, 5, false, "float_value", "1.5"),
        (1, "10:01:00", 8, 5, false, "float_value", "1.6"),
        (1, "10:02:00", 8, 5, false, "float_value", "1.7"),
        // 01:30 on 2 November in the session time zone.
        (1, "23:30:00", 8, 5, false, "float_value", "1.8"),
        (1, "11:00:00", 3, 2, false, "int_value", "7"),
        (1, "11:05:00", 3, 2, true, "int_value", "7"),
        (2, "00:10:00", 3, 10, false, "bool_value", "true"),
        (2, "00:20:00", 5, 10, false, "bool_value", "true"),
        (2, "00:30:00", 8, 5, false, "float_value", "1.9"),
    ];
    for (day, time, field, value_type, resend, column, value) in signals {
        let at = nov(day, time);
        execute(
            conn,
            &format!(
                "INSERT INTO vehicle_signal (created_at, received_at, field, vehicle_id, \
                 value_type, is_resend, {column}) VALUES ({at}, {at}, {field}, 1, \
                 {value_type}, {resend}, {value})"
            ),
        )
        .await;
    }
    // (connection, day, time, status): 1 connect, 2 disconnect.
    let events = [
        // 60 s, with connection 5 inside it.
        (1, 1, "08:00:00", 1),
        (1, 1, "08:01:00", 2),
        // Three short connections in three minutes.
        (2, 1, "08:02:00", 1),
        (2, 1, "08:02:30", 2),
        (3, 1, "08:03:00", 1),
        (3, 1, "08:03:20", 2),
        (4, 1, "08:04:00", 1),
        (4, 1, "08:04:40", 2),
        (5, 1, "08:00:30", 1),
        (5, 1, "08:00:50", 2),
        // 20 minutes across midnight.
        (6, 1, "23:50:00", 1),
        (6, 2, "00:10:00", 2),
        // Open.
        (7, 2, "01:00:00", 1),
        // A disconnect without a connect.
        (8, 2, "02:00:00", 2),
        // One connection_id for two connections of 10 s and 30 s.
        (9, 2, "03:00:00", 1),
        (9, 2, "03:00:10", 2),
        (9, 2, "03:10:00", 1),
        (9, 2, "03:10:30", 2),
    ];
    for (connection, day, time, status) in events {
        let at = nov(day, time);
        execute(
            conn,
            &format!(
                "INSERT INTO connectivity_event (connection_id, created_at, received_at, \
                 status, vehicle_id, network_interface) VALUES \
                 ('00000000-0000-0000-0000-00000000000{connection}', {at}, {at}, {status}, 1, 'wifi')"
            ),
        )
        .await;
    }
    execute(
        conn,
        &format!(
            "INSERT INTO unprocessed_record (received_at, topic, payload, reason, sqlstate) VALUES
             ({at}, '\\x61', '\\x01', 'decode_failed', NULL),
             ({at}, '\\x61', '\\x02', 'decode_failed', NULL),
             ({at}, '\\x61', '\\x03', 'database_rejected', '22021')",
            at = nov(1, "09:00:00")
        ),
    )
    .await;
    execute(
        conn,
        &format!(
            "INSERT INTO ingest_event (run_id, occurred_at, kind, dropped_count, \
             first_received_at, last_received_at) VALUES
             (gen_random_uuid(), {start}, 'started', NULL, NULL, NULL),
             (gen_random_uuid(), {start}, 'gate_full', 7, {start}, {start}),
             (gen_random_uuid(), {dropped}, 'records_dropped', 5, {dropped}, {dropped}),
             (gen_random_uuid(), {shortfall}, 'zmq_shortfall', 3, {shortfall}, {shortfall})",
            start = nov(1, "00:00:00"),
            dropped = nov(1, "12:00:00"),
            shortfall = nov(2, "01:00:00"),
        ),
    )
    .await;
}

#[tokio::test]
async fn the_usage_measurement_gives_the_expected_rows() {
    with_migrated(|db| async move {
        let schema = db.schema.as_str().to_owned();
        let mut conn = db.owner().await;
        execute(&mut conn, "SET TIME ZONE 'Europe/Helsinki'").await;

        let empty = [
            json!([]),
            json!([]),
            json!([]),
            json!([]),
            json!([]),
            json!([{"signals": 0, "resend_signals": 0, "resend_percent": null}]),
            json!([]),
            json!([]),
            json!([]),
            json!([{"signals": 0, "total_bytes": "*", "bytes_per_signal": null, "percent_of_1_gb": "*"}]),
            json!([]),
        ];
        check(&mut conn, &schema, &empty).await;

        insert_fixtures(&mut conn).await;
        let filled = [
            json!([
                {"day": "2026-11-01", "signals": 6, "usd": 0.00004},
                {"day": "2026-11-02", "signals": 3, "usd": 0.00002},
            ]),
            json!([
                {"day": "2026-11-01", "field": 8, "signals": 4, "percent_of_day": 66.67},
                {"day": "2026-11-01", "field": 3, "signals": 2, "percent_of_day": 33.33},
                {"day": "2026-11-02", "field": 3, "signals": 1, "percent_of_day": 33.33},
                {"day": "2026-11-02", "field": 5, "signals": 1, "percent_of_day": 33.33},
                {"day": "2026-11-02", "field": 8, "signals": 1, "percent_of_day": 33.33},
            ]),
            // Signals on 2 days only: no projection.
            json!([{
                "month": "2026-11-01", "signals_to_date": 9, "usd_to_date": 0.00006,
                "elapsed_days": 1.02, "days_in_month": 30, "days_with_signals": 2,
                "projected_signals": null, "projected_usd": null,
            }]),
            json!([
                {"day": "2026-11-01", "connections": 6, "closed": 6, "open": 0,
                 "without_connect": 0, "median_seconds": 35, "shortest_seconds": 20,
                 "longest_seconds": 1200},
                {"day": "2026-11-02", "connections": 4, "closed": 2, "open": 1,
                 "without_connect": 1, "median_seconds": 20, "shortest_seconds": 10,
                 "longest_seconds": 30},
            ]),
            // 60 + 30 + 20 + 40 + 600 s, then 600 + 10 + 30 s after midnight.
            json!([
                {"day": "2026-11-01", "awake_seconds": 750, "awake_hours": 0.21},
                {"day": "2026-11-02", "awake_seconds": 640, "awake_hours": 0.18},
            ]),
            json!([{"signals": 9, "resend_signals": 1, "resend_percent": 11.11}]),
            json!([
                {"reason": "decode_failed", "records": 2},
                {"reason": "database_rejected", "records": 1},
            ]),
            json!([
                {"field": 3, "invalid_signals": 1, "signals": 3, "only_invalid": false},
                {"field": 5, "invalid_signals": 1, "signals": 1, "only_invalid": true},
            ]),
            json!([
                {"day": "2026-11-01", "kind": "records_dropped", "records": 5},
                {"day": "2026-11-02", "kind": "zmq_shortfall", "records": 3},
            ]),
            json!([{"signals": 9, "total_bytes": "*", "bytes_per_signal": "*", "percent_of_1_gb": "*"}]),
            json!([
                {"day": "2026-11-01", "signals": 6, "estimated_bytes": "*"},
                {"day": "2026-11-02", "signals": 3, "estimated_bytes": "*"},
            ]),
        ];
        check(&mut conn, &schema, &filled).await;

        // A third day of signals gives the projection: 10 signals in 2.5
        // days of a 30-day month, 10 × 30 / 2.5.
        execute(
            &mut conn,
            &format!(
                "INSERT INTO vehicle_signal (created_at, received_at, field, vehicle_id, \
                 value_type, is_resend, float_value) VALUES ({at}, {at}, 8, 1, 5, false, 2.0)",
                at = nov(3, "12:00:00")
            ),
        )
        .await;
        let projected = json!([{
            "month": "2026-11-01", "signals_to_date": 10, "usd_to_date": 0.000_067,
            "elapsed_days": 2.5, "days_in_month": 30, "days_with_signals": 3,
            "projected_signals": 120, "projected_usd": 0.0008,
        }]);
        check_one(&mut conn, &schema, 3, &projected).await;
        conn.close().await.unwrap();
    })
    .await;
}

/// The ingest role has every privilege that the statements need,
/// `pg_total_relation_size` included.
#[tokio::test]
async fn the_ingest_role_can_run_every_statement() {
    with_roles(|db, roles: TestRoles| async move {
        migrate(&roles.migrator, &db.schema).await.unwrap();
        let mut conn = PgConnection::connect_with(&roles.ingest).await.unwrap();
        for statement in statements(db.schema.as_str()) {
            rows(&mut conn, &statement).await;
        }
        conn.close().await.unwrap();
    })
    .await;
}
