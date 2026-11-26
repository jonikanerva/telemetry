//! The migrations, the run check, and the constraints of the schema.

use ingest::storage::{
    ErrorCode, FailureClass, Schema, UNPROCESSED_REASONS, check_migrations, migrate,
};
use sqlx::Connection;

use crate::support::{
    count, execute, migrate_options, owner_options, sqlstate, with_migrated, with_schema,
};

#[tokio::test]
async fn migrations_build_every_table_from_an_empty_schema() {
    with_schema(|db| async move {
        let first = migrate(&migrate_options(&db.schema), &db.schema)
            .await
            .unwrap();
        assert_eq!((first.applied, first.embedded), (1, 1));
        let again = migrate(&migrate_options(&db.schema), &db.schema)
            .await
            .unwrap();
        assert_eq!((again.applied, again.embedded), (0, 1));

        let mut conn = db.owner().await;
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT relname::text FROM pg_class
             WHERE relnamespace = current_schema()::regnamespace AND relkind = 'r'
             ORDER BY 1",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap();
        assert_eq!(
            tables,
            [
                "_sqlx_migrations",
                "connectivity_event",
                "ingest_event",
                "unprocessed_record",
                "vehicle",
                "vehicle_alert",
                "vehicle_error",
                "vehicle_signal",
            ]
        );
        let indexes: Vec<(String, String)> = sqlx::query_as(
            "SELECT i.relname::text, am.amname::text
             FROM pg_index x
             JOIN pg_class i ON i.oid = x.indexrelid
             JOIN pg_am am ON am.oid = i.relam
             WHERE i.relnamespace = current_schema()::regnamespace
             ORDER BY 1",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap();
        let names: Vec<(&str, &str)> = indexes
            .iter()
            .map(|(name, method)| (name.as_str(), method.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("_sqlx_migrations_pkey", "btree"),
                ("connectivity_event_pkey", "btree"),
                ("ingest_event_key", "btree"),
                ("ingest_event_pkey", "btree"),
                ("unprocessed_record_key", "btree"),
                ("unprocessed_record_pkey", "btree"),
                ("vehicle_alert_key", "btree"),
                ("vehicle_error_key", "btree"),
                ("vehicle_pkey", "btree"),
                ("vehicle_signal_created_at_brin", "brin"),
                ("vehicle_signal_pkey", "btree"),
                ("vehicle_vin_key", "btree"),
            ]
        );
        // NULLS NOT DISTINCT on the keys with times that can be missing.
        let nulls_not_distinct: Vec<String> = sqlx::query_scalar(
            "SELECT i.relname::text FROM pg_index x JOIN pg_class i ON i.oid = x.indexrelid
             WHERE i.relnamespace = current_schema()::regnamespace AND x.indnullsnotdistinct
             ORDER BY 1",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap();
        assert_eq!(
            nulls_not_distinct,
            ["vehicle_alert_key", "vehicle_error_key"]
        );
        conn.close().await.unwrap();

        // The gate calls the check from a task of its own; the spawn proves
        // that the future is `Send`.
        let options = db.options.clone();
        let checked = {
            let options = options.clone();
            tokio::spawn(async move { check_migrations(&options).await })
                .await
                .unwrap()
        };
        assert_eq!(checked, Ok(()));
    })
    .await;
}

#[tokio::test]
async fn migrate_refuses_a_schema_that_does_not_exist() {
    with_schema(|db| async move {
        let missing = Schema::new(&format!("{}_missing", db.schema.as_str())).unwrap();
        let error = migrate(&migrate_options(&missing), &missing)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::SchemaMissing);
        assert_eq!(error.class, FailureClass::Answered);
        let mut conn = db.owner().await;
        let tables = count(
            &mut conn,
            "pg_tables WHERE schemaname = 'public'
             AND tablename IN ('vehicle', 'vehicle_signal', '_sqlx_migrations')",
        )
        .await;
        assert_eq!(tables, 0, "no table was created in public");
        let schemas = count(
            &mut conn,
            &format!("pg_namespace WHERE nspname = '{}'", missing.as_str()),
        )
        .await;
        assert_eq!(schemas, 0);
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn the_run_check_finds_missing_and_changed_migrations() {
    with_schema(|db| async move {
        let options = db.options.clone();
        let missing = check_migrations(&options).await.unwrap_err();
        assert_eq!(
            (missing.class, missing.code),
            (FailureClass::Answered, ErrorCode::MigrationMissing)
        );

        db.migrate().await;
        assert_eq!(check_migrations(&options).await, Ok(()));

        let mut conn = db.owner().await;
        execute(
            &mut conn,
            "UPDATE _sqlx_migrations SET checksum = '\\x00' WHERE version = 1",
        )
        .await;
        let changed = check_migrations(&options).await.unwrap_err();
        assert_eq!(
            (changed.class, changed.code),
            (FailureClass::Answered, ErrorCode::MigrationChanged)
        );
        // `ingest migrate` refuses a changed migration too.
        let error = migrate(&migrate_options(&db.schema), &db.schema)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::MigrationChanged);

        // A migration that the binary does not know is allowed.
        execute(&mut conn, "DELETE FROM _sqlx_migrations").await;
        execute(
            &mut conn,
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
             VALUES (9999, 'later release', true, '\\x01', 1)",
        )
        .await;
        let missing = check_migrations(&options).await.unwrap_err();
        assert_eq!(missing.code, ErrorCode::MigrationMissing);
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn an_unknown_later_migration_is_allowed() {
    with_migrated(|db| async move {
        let mut conn = db.owner().await;
        execute(
            &mut conn,
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
             VALUES (9999, 'later release', true, '\\x01', 1)",
        )
        .await;
        conn.close().await.unwrap();
        let options = db.options.clone();
        assert_eq!(check_migrations(&options).await, Ok(()));
        let outcome = migrate(&migrate_options(&db.schema), &db.schema)
            .await
            .unwrap();
        assert_eq!(outcome.applied, 0);
    })
    .await;
}

/// Inserts a vehicle with ID 1 and returns the connection.
async fn with_vehicle(db: &crate::support::TestDb) -> sqlx::PgConnection {
    let mut conn = db.owner().await;
    execute(
        &mut conn,
        "INSERT INTO vehicle (vin, first_seen_at) VALUES ('TESTVIN0000000001', now())",
    )
    .await;
    conn
}

const SIGNAL: &str =
    "INSERT INTO vehicle_signal (created_at, received_at, vehicle_id, field, value_type, is_resend";

/// Rows that break one CHECK, NOT NULL, or foreign key rule each, with
/// the expected SQLSTATE. Vehicle 1 exists.
fn bad_rows() -> Vec<(String, &'static str)> {
    let long_vin = "V".repeat(65);
    let many_audiences = format!("'{{{}}}'", vec!["1"; 65].join(","));
    vec![
        // vehicle
        ("INSERT INTO vehicle (vin, first_seen_at) VALUES ('', now())".to_owned(), "23514"),
        (format!("INSERT INTO vehicle (vin, first_seen_at) VALUES ('{long_vin}', now())"), "23514"),
        ("INSERT INTO vehicle (vin, first_seen_at) VALUES ('TESTVIN0000000002', NULL)".to_owned(), "23502"),
        // vehicle_signal: the value column follows the variant.
        (format!("{SIGNAL}, int_value) VALUES (now(), now(), 1, 1, 0, false, 1)"), "23514"),
        (format!("{SIGNAL}, int_value, float_value) VALUES (now(), now(), 1, 1, 2, false, 1, 1)"), "23514"),
        (format!("{SIGNAL}) VALUES (now(), now(), 1, 1, 2, false)"), "23514"),
        (format!("{SIGNAL}, latitude) VALUES (now(), now(), 1, 1, 7, false, 1)"), "23514"),
        (format!("{SIGNAL}, longitude) VALUES (now(), now(), 1, 1, 7, false, 1)"), "23514"),
        (format!("{SIGNAL}, int_value) VALUES (now(), now(), 1, 1, 1, false, 1)"), "23514"),
        (format!("{SIGNAL}, text_value) VALUES (now(), now(), 1, 1, 2, false, '1')"), "23514"),
        (format!("{SIGNAL}, int_value) VALUES (now(), now(), 1, 1, 4, false, 1)"), "23514"),
        (format!("{SIGNAL}, int_value) VALUES (now(), now(), 1, 1, 6, false, 1)"), "23514"),
        (format!("{SIGNAL}, int_value) VALUES (now(), now(), 1, 1, 10, false, 1)"), "23514"),
        (format!("{SIGNAL}, json_value) VALUES (now(), now(), 1, 1, 21, false, '[]')"), "23514"),
        (format!("{SIGNAL}, int_value) VALUES (now(), now(), 1, 1, 31, false, 1)"), "23514"),
        (format!("{SIGNAL}, text_value) VALUES (now(), now(), 1, 1, 8, false, 'x')"), "23514"),
        (format!("{SIGNAL}, int_value) VALUES (NULL, now(), 1, 1, 2, false, 1)"), "23502"),
        (format!("{SIGNAL}, int_value) VALUES (now(), NULL, 1, 1, 2, false, 1)"), "23502"),
        (format!("{SIGNAL}, int_value) VALUES (now(), now(), 2, 1, 2, false, 1)"), "23503"),
        // vehicle_alert: the record time is NOT NULL.
        (
            "INSERT INTO vehicle_alert (vehicle_id, name, audiences, created_at, received_at)
             VALUES (1, 'a', '{}', NULL, now())"
                .to_owned(),
            "23502",
        ),
        (
            format!(
                "INSERT INTO vehicle_alert (vehicle_id, name, audiences, created_at, received_at)
                 VALUES (1, 'a', {many_audiences}, now(), now())"
            ),
            "23514",
        ),
        // vehicle_error: the record time is NOT NULL; tags is an object.
        (
            "INSERT INTO vehicle_error (vehicle_id, name, tags, body, record_created_at, received_at)
             VALUES (1, 'e', '{}', '', NULL, now())"
                .to_owned(),
            "23502",
        ),
        (
            "INSERT INTO vehicle_error (vehicle_id, name, tags, body, record_created_at, received_at)
             VALUES (1, 'e', '[]', '', now(), now())"
                .to_owned(),
            "23514",
        ),
        // connectivity_event
        (
            "INSERT INTO connectivity_event (connection_id, created_at, received_at, status, vehicle_id, network_interface)
             VALUES (gen_random_uuid(), NULL, now(), 1, 1, '')"
                .to_owned(),
            "23502",
        ),
        // unprocessed_record
        (
            "INSERT INTO unprocessed_record (received_at, topic, payload, reason)
             VALUES (now(), '\\x00', '\\x00', 'new_reason')"
                .to_owned(),
            "23514",
        ),
        (
            "INSERT INTO unprocessed_record (received_at, topic, payload, reason, sqlstate)
             VALUES (now(), '\\x00', '\\x00', 'decode_failed', '22021')"
                .to_owned(),
            "23514",
        ),
        (
            "INSERT INTO unprocessed_record (received_at, topic, payload, reason)
             VALUES (now(), '\\x00', '\\x00', 'database_rejected')"
                .to_owned(),
            "23514",
        ),
        (
            "INSERT INTO unprocessed_record (received_at, topic, payload, reason, sqlstate)
             VALUES (now(), '\\x00', '\\x00', 'database_rejected', '22p05')"
                .to_owned(),
            "23514",
        ),
        // ingest_event
        (event("'unknown'", "NULL, NULL, NULL"), "23514"),
        (event("'gate_full'", "NULL, now(), now()"), "23514"),
        (event("'gate_full'", "1, NULL, now()"), "23514"),
        (event("'started'", "1, now(), now()"), "23514"),
        (event("'records_dropped'", "0, now(), now()"), "23514"),
        (event("'zmq_shortfall'", "1, now(), now() - interval '1 s'"), "23514"),
    ]
}

#[tokio::test]
async fn every_check_and_not_null_rule_rejects_a_bad_row() {
    with_migrated(|db| async move {
        let mut conn = with_vehicle(&db).await;
        let cases = bad_rows();
        for (sql, expected) in &cases {
            assert_eq!(
                sqlstate(&mut conn, sql).await.as_deref(),
                Some(*expected),
                "{sql}"
            );
        }
        // The rules accept the valid forms.
        for sql in [
            format!("{SIGNAL}, int_value) VALUES (now(), now(), 1, 1, 8, false, 4)"),
            format!("{SIGNAL}, latitude, longitude) VALUES (now(), now(), 1, 2, 7, false, 1, 2)"),
            format!("{SIGNAL}, json_value) VALUES (now(), now(), 1, 3, 39, false, '{{}}')"),
            format!("{SIGNAL}, float_value) VALUES (now(), now(), 1, 4, 5, false, 'NaN')"),
            event("'gate_full'", "1, now(), now()"),
            event("'started'", "NULL, NULL, NULL"),
        ] {
            assert_eq!(sqlstate(&mut conn, &sql).await, None, "{sql}");
        }
        conn.close().await.unwrap();
    })
    .await;
}

fn event(kind: &str, window: &str) -> String {
    format!(
        "INSERT INTO ingest_event (run_id, occurred_at, kind, dropped_count, first_received_at, last_received_at)
         VALUES (gen_random_uuid(), clock_timestamp(), {kind}, {window})"
    )
}

#[tokio::test]
async fn every_unique_rule_rejects_a_second_copy() {
    with_migrated(|db| async move {
        let mut conn = with_vehicle(&db).await;
        let run = "'00000000-0000-0000-0000-000000000001'";
        let cases = [
            "INSERT INTO vehicle (vin, first_seen_at) VALUES ('TESTVIN0000000003', now())".to_owned(),
            // The signal key excludes the value, `is_resend`, and `received_at`.
            format!("{SIGNAL}, int_value) VALUES ('2026-11-01T00:00:00Z', now(), 1, 8, 2, false, 1)"),
            // An active alert: started_at and ended_at NULL.
            "INSERT INTO vehicle_alert (vehicle_id, name, audiences, started_at, ended_at, created_at, received_at)
             VALUES (1, 'TestAlert', '{1,2}', NULL, NULL, now(), now())"
                .to_owned(),
            // An error item without created_at.
            "INSERT INTO vehicle_error (vehicle_id, created_at, name, tags, body, record_created_at, received_at)
             VALUES (1, NULL, 'test_error', '{\"a\": \"b\"}', 'body', now(), now())"
                .to_owned(),
            "INSERT INTO connectivity_event (connection_id, created_at, received_at, status, vehicle_id, network_interface)
             VALUES ('00000000-0000-0000-0000-000000000002', '2026-11-01T00:00:00Z', now(), 1, 1, 'wifi')"
                .to_owned(),
            "INSERT INTO unprocessed_record (received_at, topic, payload, reason)
             VALUES (clock_timestamp(), '\\x00ff', '\\x0102', 'unknown_topic')"
                .to_owned(),
            format!(
                "INSERT INTO ingest_event (run_id, occurred_at, kind)
                 VALUES ({run}, '2026-11-01T00:00:00Z', 'started')"
            ),
        ];
        for sql in &cases {
            assert_eq!(sqlstate(&mut conn, sql).await, None, "first copy: {sql}");
            let second = sql
                .replace("VALUES ('2026-11-01T00:00:00Z', now(), 1, 8, 2, false, 1)", "VALUES ('2026-11-01T00:00:00Z', now(), 1, 8, 2, true, 2)");
            assert_eq!(sqlstate(&mut conn, &second).await.as_deref(), Some("23505"), "second copy: {second}");
        }
        // Another state of the alert is a new row: other audiences, an end time.
        for sql in [
            "INSERT INTO vehicle_alert (vehicle_id, name, audiences, started_at, ended_at, created_at, received_at)
             VALUES (1, 'TestAlert', '{1}', NULL, NULL, now(), now())",
            "INSERT INTO vehicle_alert (vehicle_id, name, audiences, started_at, ended_at, created_at, received_at)
             VALUES (1, 'TestAlert', '{1,2}', NULL, now(), now(), now())",
        ] {
            assert_eq!(sqlstate(&mut conn, sql).await, None, "{sql}");
        }
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn the_reason_check_allows_every_reason_of_the_code() {
    with_migrated(|db| async move {
        let mut conn = db.owner().await;
        for reason in UNPROCESSED_REASONS {
            let sqlstate_value = if reason == "database_rejected" {
                "'22021'"
            } else {
                "NULL"
            };
            let sql = format!(
                "INSERT INTO unprocessed_record (received_at, topic, payload, reason, sqlstate)
                 VALUES (now(), '\\x00', '\\x00', '{reason}', {sqlstate_value})"
            );
            assert_eq!(sqlstate(&mut conn, &sql).await, None, "{reason}");
        }
        assert_eq!(count(&mut conn, "unprocessed_record").await, 7);
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn the_run_check_without_a_database_is_no_answer() {
    // Port 1 on the loopback interface refuses every connection.
    let options = owner_options(&Schema::raw()).host("127.0.0.1").port(1);
    let error = check_migrations(&options).await.unwrap_err();
    assert_eq!(error.class, FailureClass::NoAnswer, "{error}");
}

#[tokio::test]
async fn a_silent_server_stops_every_call_at_its_deadline() {
    // The listener accepts TCP connections (the operating system does it)
    // and never answers, as a stalled peer.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let options = migrate_options(&Schema::raw()).host("127.0.0.1").port(port);
    let raw = Schema::raw();
    let started = std::time::Instant::now();
    let (connected, migrated, checked) = tokio::join!(
        ingest::storage::connect(&options),
        migrate(&options, &raw),
        check_migrations(&options),
    );
    let elapsed = started.elapsed();
    let timeout = (FailureClass::NoAnswer, ErrorCode::Timeout);
    let error = connected.unwrap_err();
    assert_eq!((error.class, error.code), timeout);
    let error = migrated.unwrap_err();
    assert_eq!((error.class, error.code), timeout);
    let error = checked.unwrap_err();
    assert_eq!((error.class, error.code), timeout);
    // The connect timeout is 15 s.
    assert!(elapsed >= std::time::Duration::from_secs(14), "{elapsed:?}");
    assert!(elapsed < std::time::Duration::from_secs(25), "{elapsed:?}");
    drop(listener);
}
