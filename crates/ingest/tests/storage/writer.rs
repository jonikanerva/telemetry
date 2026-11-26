//! The batch writer: every table, resends, conflicts, a poisoned record,
//! long values, and competing writers.

use ingest::record::{Location, SignalValue, UnprocessedReason};
use ingest::storage::{FailureClass, WriteOutcome, Writer};
use jiff::Timestamp;
use serde_json::json;
use sqlx::{Connection, PgConnection, Row};

use crate::support::{
    BASE, Message, VIN, alert, alerts, at, connectivity, count, enum_value, error_item, errors,
    execute, queued, unprocessed, vehicle_data, with_migrated, with_vin,
};

/// One stored signal: field, value type, and the value columns as text.
#[derive(Debug, PartialEq)]
struct StoredSignal {
    field: i32,
    value_type: i16,
    int_value: Option<i64>,
    float_bits: Option<u64>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    bool_value: Option<bool>,
    text_value: Option<String>,
    json_value: Option<serde_json::Value>,
    is_resend: bool,
}

async fn signals(conn: &mut PgConnection) -> Vec<StoredSignal> {
    sqlx::query(
        "SELECT field, value_type, int_value, float_value, latitude, longitude, bool_value,
                text_value, json_value, is_resend
         FROM vehicle_signal ORDER BY field, created_at",
    )
    .fetch_all(conn)
    .await
    .unwrap()
    .into_iter()
    .map(|row| StoredSignal {
        field: row.get(0),
        value_type: row.get(1),
        int_value: row.get(2),
        float_bits: row.get::<Option<f64>, _>(3).map(f64::to_bits),
        latitude: row.get(4),
        longitude: row.get(5),
        bool_value: row.get(6),
        text_value: row.get(7),
        json_value: row.get(8),
        is_resend: row.get(9),
    })
    .collect()
}

fn stored(field: i32, value_type: i16) -> StoredSignal {
    StoredSignal {
        field,
        value_type,
        int_value: None,
        float_bits: None,
        latitude: None,
        longitude: None,
        bool_value: None,
        text_value: None,
        json_value: None,
        is_resend: false,
    }
}

/// The unprocessed rows: topic, payload, reason, SQLSTATE.
async fn unprocessed_rows(
    conn: &mut PgConnection,
) -> Vec<(Vec<u8>, Vec<u8>, String, Option<String>)> {
    sqlx::query_as("SELECT topic, payload, reason, sqlstate FROM unprocessed_record ORDER BY id")
        .fetch_all(conn)
        .await
        .unwrap()
}

async fn write(db: &crate::support::TestDb, messages: &[Message]) -> WriteOutcome {
    let pool = db.pool();
    let mut writer = Writer::new(pool.clone());
    let outcome = writer.write(&queued(messages), &mut || {}).await.unwrap();
    pool.close().await;
    outcome
}

fn all_variants() -> Vec<(i32, SignalValue)> {
    let object = |pairs: &[(&str, serde_json::Value)]| {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect::<serde_json::Map<_, _>>()
    };
    vec![
        (1, SignalValue::Text("12.3".to_owned())),
        (2, SignalValue::Int32(-42)),
        (3, SignalValue::Int64(i64::MAX)),
        (4, SignalValue::Float32(0.1)),
        (5, SignalValue::Float64(-273.15)),
        (6, SignalValue::Bool(false)),
        (
            7,
            SignalValue::Location(Location {
                latitude: 12.5,
                longitude: -45.25,
            }),
        ),
        (8, SignalValue::Invalid(true)),
        // `automatic_emergency_braking_state_value` (60), number 7.
        (9, enum_value(60, 7)),
        (
            10,
            SignalValue::Doors(object(&[
                ("DriverFront", json!(true)),
                ("TrunkRear", json!(false)),
            ])),
        ),
        (
            11,
            SignalValue::Time(object(&[
                ("hour", json!(23)),
                ("minute", json!(0)),
                ("second", json!(59)),
            ])),
        ),
        (
            12,
            SignalValue::TireLocation(object(&[("front_left", json!(true))])),
        ),
        // A field that the vendored schema does not know stays a number.
        (99_999, SignalValue::Int32(1)),
    ]
}

#[tokio::test]
async fn every_variant_is_stored_in_its_column() {
    with_migrated(|db| async move {
        let mut record = vehicle_data(0, all_variants());
        if let ingest::record::Decoded::VehicleData(data) = &mut record {
            data.received_at = at(BASE + 2, 123_456_789);
        }
        let outcome = write(&db, &[Message::new(record)]).await;
        assert_eq!(outcome.rows_inserted, 13);
        assert_eq!(outcome.transactions, 1);

        let mut conn = db.owner().await;
        let mut expected = vec![
            StoredSignal {
                text_value: Some("12.3".to_owned()),
                ..stored(1, 1)
            },
            StoredSignal {
                int_value: Some(-42),
                ..stored(2, 2)
            },
            StoredSignal {
                int_value: Some(i64::MAX),
                ..stored(3, 3)
            },
            StoredSignal {
                float_bits: Some(f64::from(0.1_f32).to_bits()),
                ..stored(4, 4)
            },
            StoredSignal {
                float_bits: Some((-273.15_f64).to_bits()),
                ..stored(5, 5)
            },
            StoredSignal {
                bool_value: Some(false),
                ..stored(6, 6)
            },
            StoredSignal {
                latitude: Some(12.5),
                longitude: Some(-45.25),
                ..stored(7, 7)
            },
            StoredSignal {
                bool_value: Some(true),
                ..stored(8, 10)
            },
            StoredSignal {
                int_value: Some(7),
                ..stored(9, 60)
            },
            StoredSignal {
                json_value: Some(json!({ "DriverFront": true, "TrunkRear": false })),
                ..stored(10, 21)
            },
            StoredSignal {
                json_value: Some(json!({ "hour": 23, "minute": 0, "second": 59 })),
                ..stored(11, 31)
            },
            StoredSignal {
                json_value: Some(json!({ "front_left": true })),
                ..stored(12, 39)
            },
            StoredSignal {
                int_value: Some(1),
                ..stored(99_999, 2)
            },
        ];
        expected.sort_by_key(|row| row.field);
        assert_eq!(signals(&mut conn).await, expected);

        // The receive time is truncated to microseconds; the vehicle row
        // has the VIN and the first receive time.
        let received: Vec<Timestamp> = sqlx::query_scalar::<_, jiff_sqlx::Timestamp>(
            "SELECT DISTINCT received_at FROM vehicle_signal",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap()
        .into_iter()
        .map(jiff_sqlx::Timestamp::to_jiff)
        .collect();
        assert_eq!(received, [at(BASE + 2, 123_456_000)]);
        let (vin, first_seen): (String, jiff_sqlx::Timestamp) =
            sqlx::query_as("SELECT vin, first_seen_at FROM vehicle")
                .fetch_one(&mut conn)
                .await
                .unwrap();
        assert_eq!(vin, VIN);
        assert_eq!(first_seen.to_jiff(), at(BASE + 2, 123_456_000));
        conn.close().await.unwrap();
    })
    .await;
}

/// A batch with a record of each type and a message without rows.
fn mixed_batch() -> Vec<Message> {
    vec![
        Message::new(vehicle_data(
            0,
            vec![(8, SignalValue::Int32(1)), (9, SignalValue::Float64(2.5))],
        )),
        Message::new(alerts(
            0,
            vec![alert("TestAlert_a001", vec![1, 2], Some(-10), None)],
        )),
        Message::new(errors(
            0,
            vec![error_item(None, "test_error", &[("k", "v")], "body")],
        )),
        Message::new(connectivity(0, 1, 1, "wifi")),
        Message::new(unprocessed(UnprocessedReason::DecodeFailed, 0)).with_payload(b"\x08\xff"),
    ]
}

#[tokio::test]
async fn a_resent_batch_stores_nothing_twice() {
    with_migrated(|db| async move {
        let batch = mixed_batch();
        let first = write(&db, &batch).await;
        assert_eq!(
            first,
            WriteOutcome {
                attempts: 1,
                transactions: 1,
                rows_inserted: 5,
                unprocessed_inserted: 1,
                ..WriteOutcome::default()
            }
        );
        // The same records again, for example after a lost commit answer.
        let second = write(&db, &batch).await;
        assert_eq!(
            second,
            WriteOutcome {
                attempts: 1,
                transactions: 1,
                duplicate_rows: 5,
                ..WriteOutcome::default()
            }
        );
        let mut conn = db.owner().await;
        for (table, rows) in [
            ("vehicle", 1),
            ("vehicle_signal", 2),
            ("vehicle_alert", 1),
            ("vehicle_error", 1),
            ("connectivity_event", 1),
            ("unprocessed_record", 1),
        ] {
            assert_eq!(count(&mut conn, table).await, rows, "{table}");
        }
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_resend_from_the_vehicle_keeps_the_first_copy() {
    with_migrated(|db| async move {
        write(
            &db,
            &[Message::new(vehicle_data(
                0,
                vec![(8, SignalValue::Int32(1))],
            ))],
        )
        .await;
        let mut resend = vehicle_data(0, vec![(8, SignalValue::Int32(1))]);
        if let ingest::record::Decoded::VehicleData(data) = &mut resend {
            data.is_resend = true;
            data.received_at = at(BASE + 3_600, 0);
        }
        let outcome = write(&db, &[Message::new(resend)]).await;
        assert_eq!((outcome.rows_inserted, outcome.duplicate_rows), (0, 1));
        assert_eq!(outcome.conflicting_records, 0);
        let mut conn = db.owner().await;
        let (is_resend, received): (bool, jiff_sqlx::Timestamp) =
            sqlx::query_as("SELECT is_resend, received_at FROM vehicle_signal")
                .fetch_one(&mut conn)
                .await
                .unwrap();
        assert!(!is_resend);
        assert_eq!(received.to_jiff(), at(BASE + 2, 0));
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn another_value_for_a_stored_key_is_a_conflicting_duplicate() {
    with_migrated(|db| async move {
        write(
            &db,
            &[Message::new(vehicle_data(
                0,
                vec![(8, SignalValue::Int32(1))],
            ))],
        )
        .await;
        let newer = Message::new(vehicle_data(
            0,
            vec![(8, SignalValue::Int32(2)), (9, SignalValue::Bool(true))],
        ))
        .with_payload(b"newer record bytes");
        let outcome = write(&db, std::slice::from_ref(&newer)).await;
        assert_eq!(outcome.conflicting_records, 1);
        assert_eq!(outcome.rows_inserted, 1, "the other datum is stored");
        assert_eq!(outcome.unprocessed_inserted, 1);

        let mut conn = db.owner().await;
        let values: Vec<(i32, Option<i64>)> =
            sqlx::query_as("SELECT field, int_value FROM vehicle_signal ORDER BY field")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(values, [(8, Some(1)), (9, None)], "the first copy wins");
        assert_eq!(
            unprocessed_rows(&mut conn).await,
            [(
                b"tesla_telemetry_V".to_vec(),
                b"newer record bytes".to_vec(),
                "conflicting_duplicate".to_owned(),
                None
            )]
        );
        // A resend of the conflicting record adds nothing.
        let again = write(&db, std::slice::from_ref(&newer)).await;
        assert_eq!(
            (again.conflicting_records, again.unprocessed_inserted),
            (1, 0)
        );
        assert_eq!(count(&mut conn, "unprocessed_record").await, 1);
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn another_value_in_the_same_batch_is_a_conflicting_duplicate() {
    with_migrated(|db| async move {
        let batch = [
            Message::new(vehicle_data(0, vec![(8, SignalValue::Int32(1))])).with_payload(b"first"),
            Message::new(vehicle_data(0, vec![(8, SignalValue::Int32(1))])).with_payload(b"copy"),
            Message::new(vehicle_data(0, vec![(8, SignalValue::Int64(1))]))
                .with_payload(b"other type"),
        ];
        let outcome = write(&db, &batch).await;
        assert_eq!(outcome.rows_inserted, 1);
        assert_eq!(outcome.duplicate_rows, 1);
        assert_eq!(outcome.conflicting_records, 1);
        let mut conn = db.owner().await;
        let rows = unprocessed_rows(&mut conn).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, b"other type");
        assert_eq!(rows[0].2, "conflicting_duplicate");
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn nan_resends_are_duplicates_and_signed_zeros_conflict() {
    with_migrated(|db| async move {
        let nan = f64::from_bits(0x7ff8_0000_0000_0001);
        let first = [Message::new(vehicle_data(
            0,
            vec![
                (1, SignalValue::Float64(nan)),
                (2, SignalValue::Float64(0.0)),
            ],
        ))];
        write(&db, &first).await;
        // The same NaN bits again: a duplicate, also against the table.
        let resend = [Message::new(vehicle_data(
            0,
            vec![(1, SignalValue::Float64(nan))],
        ))];
        let outcome = write(&db, &resend).await;
        assert_eq!(
            (outcome.duplicate_rows, outcome.conflicting_records),
            (1, 0)
        );
        // Another NaN and a negative zero: conflicts.
        let other_nan = f64::from_bits(0x7ff8_0000_0000_0002);
        let changed = [
            Message::new(vehicle_data(0, vec![(1, SignalValue::Float64(other_nan))]))
                .with_payload(b"a"),
            Message::new(vehicle_data(0, vec![(2, SignalValue::Float64(-0.0))])).with_payload(b"b"),
        ];
        let outcome = write(&db, &changed).await;
        assert_eq!(outcome.conflicting_records, 2);
        let mut conn = db.owner().await;
        let stored = signals(&mut conn).await;
        assert_eq!(
            stored[0].float_bits,
            Some(nan.to_bits()),
            "PostgreSQL keeps the bits"
        );
        assert_eq!(stored[1].float_bits, Some(0.0_f64.to_bits()));
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_poisoned_record_is_isolated_and_the_others_commit() {
    with_migrated(|db| async move {
        let mut batch: Vec<Message> = (0..9)
            .map(|index| {
                Message::new(vehicle_data(index, vec![(8, SignalValue::Int32(1))]))
                    .with_payload(format!("record {index}").as_bytes())
            })
            .collect();
        batch[4] = Message::new(vehicle_data(
            4,
            vec![
                (8, SignalValue::Int32(1)),
                (9, SignalValue::Text("a\0b".to_owned())),
            ],
        ))
        .with_payload(b"record 4");
        let outcome = write(&db, &batch).await;
        assert_eq!(outcome.rejected_records, 1);
        assert_eq!(outcome.rows_inserted, 8);
        assert!(outcome.transactions > 2, "{outcome:?}");

        let mut conn = db.owner().await;
        let created: Vec<i64> = sqlx::query_scalar(
            "SELECT extract(epoch FROM created_at)::bigint - $1 FROM vehicle_signal ORDER BY 1",
        )
        .bind(BASE)
        .fetch_all(&mut conn)
        .await
        .unwrap();
        assert_eq!(created, [0, 1, 2, 3, 5, 6, 7, 8], "no row of record 4");
        assert_eq!(
            unprocessed_rows(&mut conn).await,
            [(
                b"tesla_telemetry_V".to_vec(),
                b"record 4".to_vec(),
                "database_rejected".to_owned(),
                Some("22021".to_owned())
            )]
        );
        // The same batch again stores nothing new.
        let again = write(&db, &batch).await;
        assert_eq!((again.rows_inserted, again.unprocessed_inserted), (0, 0));
        assert_eq!(again.rejected_records, 1);
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn rejected_values_keep_their_sqlstate_and_their_bytes() {
    with_migrated(|db| async move {
        let long_vin = "V".repeat(65);
        let mut bad_tags = errors(
            0,
            vec![error_item(None, "test_error", &[("k", "a\0b")], "body")],
        );
        if let ingest::record::Decoded::Errors(record) = &mut bad_tags {
            record.unknown_fields = true;
        }
        let cases = [
            (
                with_vin(vehicle_data(0, vec![(8, SignalValue::Int32(1))]), ""),
                "23514",
            ),
            (
                with_vin(vehicle_data(0, vec![(8, SignalValue::Int32(1))]), &long_vin),
                "23514",
            ),
            (
                with_vin(
                    vehicle_data(0, vec![(8, SignalValue::Int32(1))]),
                    "TESTVIN\0",
                ),
                "22021",
            ),
            (
                alerts(0, vec![alert("Test\0Alert", vec![1], None, None)]),
                "22021",
            ),
            (
                alerts(0, vec![alert("TestAlert", vec![1; 65], None, None)]),
                "23514",
            ),
            (bad_tags, "22P05"),
            (connectivity(0, 9, 1, "wi\0fi"), "22021"),
        ];
        // One batch: a good record before each bad one. The split isolates
        // every bad record; the good ones commit.
        let mut batch = Vec::new();
        let mut expected_rows = Vec::new();
        for (index, (decoded, expected)) in cases.into_iter().enumerate() {
            let created = i64::try_from(index).unwrap();
            batch.push(Message::new(vehicle_data(
                created,
                vec![(8, SignalValue::Int32(1))],
            )));
            let payload = format!("bad {index}");
            batch.push(Message::new(decoded).with_payload(payload.as_bytes()));
            expected_rows.push((
                payload.clone(),
                "database_rejected".to_owned(),
                Some(expected.to_owned()),
            ));
            if index == 5 {
                // The decode reason of the record is kept as well.
                expected_rows.push((payload, "unknown_fields".to_owned(), None));
            }
        }
        let outcome = write(&db, &batch).await;
        assert_eq!(outcome.rejected_records, 7);
        assert_eq!(outcome.rows_inserted, 7, "every good record commits");
        let mut conn = db.owner().await;
        let rows: Vec<(Vec<u8>, String, Option<String>)> = sqlx::query_as(
            "SELECT payload, reason, sqlstate FROM unprocessed_record ORDER BY payload, reason",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap();
        let rows: Vec<(String, String, Option<String>)> = rows
            .into_iter()
            .map(|(payload, reason, state)| (String::from_utf8(payload).unwrap(), reason, state))
            .collect();
        assert_eq!(rows, expected_rows);
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn every_wire_number_and_a_short_vin_are_stored_as_sent() {
    with_migrated(|db| async move {
        // The enum number -1 of an `int32` enum: a varint of the
        // sign-extended 64 bits.
        let minus_one = u64::from_ne_bytes((-1_i64).to_ne_bytes());
        let batch = [
            Message::new(with_vin(
                vehicle_data(
                    0,
                    vec![
                        (i32::MAX, SignalValue::Int32(1)),
                        (i32::MIN, SignalValue::Int32(2)),
                        (0, enum_value(8, minus_one)),
                    ],
                ),
                "TESTVIN",
            )),
            Message::new(alerts(
                0,
                vec![alert("TestAlert", vec![i32::MIN, i32::MAX, -1], None, None)],
            )),
            Message::new(connectivity(0, 3, i32::MIN, "")),
            Message::new(connectivity(0, 4, i32::MAX, "")),
        ];
        let outcome = write(&db, &batch).await;
        assert_eq!((outcome.rows_inserted, outcome.rejected_records), (6, 0));
        let mut conn = db.owner().await;
        let fields: Vec<(i32, Option<i64>)> =
            sqlx::query_as("SELECT field, int_value FROM vehicle_signal ORDER BY field")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(
            fields,
            [(i32::MIN, Some(2)), (0, Some(-1)), (i32::MAX, Some(1))]
        );
        let audiences: Vec<i32> = sqlx::query_scalar("SELECT audiences FROM vehicle_alert")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(audiences, [i32::MIN, i32::MAX, -1]);
        let statuses: Vec<i32> =
            sqlx::query_scalar("SELECT status FROM connectivity_event ORDER BY status")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(statuses, [i32::MIN, i32::MAX]);
        // A VIN of another form than 17 characters is the identity as sent.
        let vins: Vec<String> = sqlx::query_scalar("SELECT vin FROM vehicle ORDER BY id")
            .fetch_all(&mut conn)
            .await
            .unwrap();
        assert_eq!(vins, ["TESTVIN", VIN]);
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_reason_without_its_migration_keeps_the_record_as_rejected() {
    with_migrated(|db| async move {
        // A schema whose CHECK list lacks `unknown_fields`, as when a
        // release adds a reason without the migration.
        let mut conn = db.owner().await;
        execute(
            &mut conn,
            "ALTER TABLE unprocessed_record DROP CONSTRAINT unprocessed_record_reason;
             ALTER TABLE unprocessed_record ADD CONSTRAINT unprocessed_record_reason CHECK (
                 reason IN ('decode_failed', 'unknown_topic', 'missing_created_at',
                            'unknown_value_variant', 'conflicting_duplicate', 'database_rejected'))",
        )
        .await;
        let mut newer = vehicle_data(1, vec![(9, SignalValue::Int32(1))]);
        if let ingest::record::Decoded::VehicleData(data) = &mut newer {
            data.unknown_fields = true;
        }
        let batch = [
            Message::new(vehicle_data(0, vec![(8, SignalValue::Int32(1))])),
            Message::new(newer).with_payload(b"newer schema"),
        ];
        let outcome = write(&db, &batch).await;
        assert_eq!((outcome.rows_inserted, outcome.rejected_records), (1, 1));
        let rows = unprocessed_rows(&mut conn).await;
        assert_eq!(
            rows,
            [(
                b"tesla_telemetry_V".to_vec(),
                b"newer schema".to_vec(),
                "database_rejected".to_owned(),
                Some("23514".to_owned())
            )]
        );
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn long_values_and_odd_topics_are_stored() {
    with_migrated(|db| async move {
        let long = "x".repeat(10_000);
        let body = "b".repeat(100_000);
        let topic: Vec<u8> = [
            b"tesla\0telemetry_".as_slice(),
            &[0xff, 0xfe],
            &[b't'; 10_000],
        ]
        .concat();
        let batch = [
            Message::new(vehicle_data(0, vec![(1, SignalValue::Text(body.clone()))])),
            Message::new(alerts(
                0,
                vec![alert(&long, vec![0, 1, 2, 3], Some(1), Some(2))],
            )),
            Message::new(errors(
                0,
                vec![error_item(Some(1), &long, &[(&long, &long)], &body)],
            )),
            Message::new(unprocessed(UnprocessedReason::UnknownTopic, 0)).with_topic(&topic),
        ];
        let outcome = write(&db, &batch).await;
        assert_eq!((outcome.rows_inserted, outcome.rejected_records), (3, 0));
        // The same rows again are duplicates by their md5() keys.
        let again = write(&db, &batch).await;
        assert_eq!((again.rows_inserted, again.duplicate_rows), (0, 3));
        let mut conn = db.owner().await;
        let stored_topic: Vec<u8> = sqlx::query_scalar("SELECT topic FROM unprocessed_record")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(stored_topic, topic, "every byte of the topic is kept");
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn alert_states_and_error_items_follow_their_keys() {
    with_migrated(|db| async move {
        let active = || alert("TestAlert_a001", vec![1, 2], Some(-10), None);
        // Replays of an active alert, with a new record time each time.
        write(&db, &[Message::new(alerts(0, vec![active()]))]).await;
        let replay = write(&db, &[Message::new(alerts(60, vec![active()]))]).await;
        assert_eq!((replay.rows_inserted, replay.duplicate_rows), (0, 1));
        // The end of the alert and other audiences are new states.
        let ended = alert("TestAlert_a001", vec![1, 2], Some(-10), Some(30));
        let other = alert("TestAlert_a001", vec![2], Some(-10), None);
        let outcome = write(&db, &[Message::new(alerts(120, vec![ended, other]))]).await;
        assert_eq!(outcome.rows_inserted, 2);
        // Error items without a time: a replay is a duplicate.
        let item = || error_item(None, "test_error", &[("a", "1"), ("b", "2")], "body");
        write(&db, &[Message::new(errors(0, vec![item()]))]).await;
        let replay = write(&db, &[Message::new(errors(60, vec![item(), item()]))]).await;
        assert_eq!((replay.rows_inserted, replay.duplicate_rows), (0, 2));
        let mut conn = db.owner().await;
        assert_eq!(count(&mut conn, "vehicle_alert").await, 3);
        assert_eq!(count(&mut conn, "vehicle_error").await, 1);
        let audiences: Vec<Vec<i32>> = sqlx::query_scalar(
            "SELECT audiences FROM vehicle_alert ORDER BY ended_at NULLS FIRST, audiences",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap();
        assert_eq!(audiences, [vec![1, 2], vec![2], vec![1, 2]]);
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn connectivity_events_are_idempotent_and_conflicts_are_kept() {
    with_migrated(|db| async move {
        write(&db, &[Message::new(connectivity(0, 7, 1, "wifi"))]).await;
        let resend = write(&db, &[Message::new(connectivity(0, 7, 1, "wifi"))]).await;
        assert_eq!((resend.duplicate_rows, resend.conflicting_records), (1, 0));
        let other = write(
            &db,
            &[Message::new(connectivity(0, 7, 1, "cellular")).with_payload(b"cell")],
        )
        .await;
        assert_eq!(other.conflicting_records, 1);
        // The same connection ID later is a new event.
        let later = write(&db, &[Message::new(connectivity(5, 7, 2, ""))]).await;
        assert_eq!(later.rows_inserted, 1);
        let mut conn = db.owner().await;
        assert_eq!(count(&mut conn, "connectivity_event").await, 2);
        assert_eq!(unprocessed_rows(&mut conn).await[0].1, b"cell");
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_transaction_holds_at_most_5000_signal_rows() {
    with_migrated(|db| async move {
        let many = |created| {
            vehicle_data(
                created,
                (0..3_000)
                    .map(|field| (field, SignalValue::Int32(field)))
                    .collect(),
            )
        };
        let outcome = write(&db, &[Message::new(many(0)), Message::new(many(1))]).await;
        assert_eq!((outcome.transactions, outcome.rows_inserted), (2, 6_000));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn competing_writers_store_each_row_once() {
    with_migrated(|db| async move {
        // Two writers with their own pools, as two overlapping deployments.
        let batch: Vec<Message> = (0..50)
            .map(|index| {
                Message::new(vehicle_data(
                    index,
                    (0..20)
                        .map(|field| (field, SignalValue::Int64(index * 100 + i64::from(field))))
                        .collect(),
                ))
            })
            .collect();
        let reversed: Vec<Message> = batch
            .iter()
            .rev()
            .map(|message| Message {
                topic: message.topic.clone(),
                payload: message.payload.clone(),
                decoded: message.decoded.clone(),
            })
            .collect();
        let first_pool = db.pool();
        let second_pool = db.pool();
        // Each writer runs in a task of its own, as in the pipeline. The
        // spawn also proves that the future of `Writer::write` is `Send`.
        let spawn = |messages: Vec<Message>, pool: sqlx::PgPool| {
            tokio::spawn(async move {
                let mut writer = Writer::new(pool);
                writer.write(&queued(&messages), &mut || {}).await
            })
        };
        let first = spawn(batch, first_pool.clone());
        let second = spawn(reversed, second_pool.clone());
        let a = first.await.unwrap().unwrap();
        let b = second.await.unwrap().unwrap();
        assert_eq!(a.rows_inserted + b.rows_inserted, 1_000);
        assert_eq!(a.duplicate_rows + b.duplicate_rows, 1_000);
        assert_eq!(a.conflicting_records + b.conflicting_records, 0);
        first_pool.close().await;
        second_pool.close().await;
        let mut conn = db.owner().await;
        assert_eq!(count(&mut conn, "vehicle_signal").await, 1_000);
        assert_eq!(count(&mut conn, "vehicle").await, 1);
        assert_eq!(count(&mut conn, "unprocessed_record").await, 0);
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn the_writer_creates_a_vehicle_once_and_keeps_its_id() {
    with_migrated(|db| async move {
        let pool = db.pool();
        let mut writer = Writer::new(pool.clone());
        for created in 0..3 {
            let message = Message::new(vehicle_data(created, vec![(8, SignalValue::Int32(1))]));
            writer.write(&[message.queued()], &mut || {}).await.unwrap();
        }
        // A new writer, as after a restart, finds the stored VIN.
        let mut writer = Writer::new(pool.clone());
        let message = Message::new(vehicle_data(3, vec![(8, SignalValue::Int32(1))]));
        writer.write(&[message.queued()], &mut || {}).await.unwrap();
        let other = Message::new(with_vin(
            vehicle_data(0, vec![(8, SignalValue::Int32(1))]),
            "TESTVIN0000000002",
        ));
        writer.write(&[other.queued()], &mut || {}).await.unwrap();
        pool.close().await;
        let mut conn = db.owner().await;
        let ids: Vec<(i16, String)> = sqlx::query_as("SELECT id, vin FROM vehicle ORDER BY id")
            .fetch_all(&mut conn)
            .await
            .unwrap();
        // The select before the insert takes no identity value for a
        // known VIN, so the second vehicle gets ID 2.
        assert_eq!(
            ids,
            [(1, VIN.to_owned()), (2, "TESTVIN0000000002".to_owned())]
        );
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn the_first_copy_wins_across_a_split() {
    with_migrated(|db| async move {
        // Three records: A and B hold the same key with other values, and P
        // between them fails. The split writes [A] first, then [P, B], then
        // [P] and [B]. A is stored; B is a conflict; P is rejected.
        let batch = [
            Message::new(vehicle_data(0, vec![(8, SignalValue::Int32(1))])).with_payload(b"A"),
            Message::new(vehicle_data(
                5,
                vec![(9, SignalValue::Text("a\0b".to_owned()))],
            ))
            .with_payload(b"P"),
            Message::new(vehicle_data(0, vec![(8, SignalValue::Int32(2))])).with_payload(b"B"),
        ];
        let outcome = write(&db, &batch).await;
        assert_eq!(
            (
                outcome.rows_inserted,
                outcome.conflicting_records,
                outcome.rejected_records
            ),
            (1, 1, 1)
        );
        let mut conn = db.owner().await;
        let stored: Vec<(i32, Option<i64>)> =
            sqlx::query_as("SELECT field, int_value FROM vehicle_signal")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(stored, [(8, Some(1))], "the value of A, the first copy");
        let reasons: Vec<(Vec<u8>, String)> =
            sqlx::query_as("SELECT payload, reason FROM unprocessed_record ORDER BY payload")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(
            reasons,
            [
                (b"B".to_vec(), "conflicting_duplicate".to_owned()),
                (b"P".to_vec(), "database_rejected".to_owned()),
            ]
        );
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_failed_write_says_which_records_are_stored() {
    with_migrated(|db| async move {
        // Two chunks: two records of 3,000 signals each, and an alert record
        // in the second chunk. The alert table does not exist, so the second
        // chunk fails with an answered error after the first committed.
        let many = |created| {
            vehicle_data(
                created,
                (0..3_000)
                    .map(|field| (field, SignalValue::Int32(field)))
                    .collect(),
            )
        };
        let batch = [
            Message::new(many(0)),
            Message::new(many(1)),
            Message::new(alerts(2, vec![alert("TestAlert", vec![1], None, None)])),
        ];
        let mut conn = db.owner().await;
        execute(&mut conn, "DROP TABLE vehicle_alert").await;
        let pool = db.pool();
        let mut writer = Writer::new(pool.clone());
        let error = writer.write(&queued(&batch), &mut || {}).await.unwrap_err();
        assert_eq!(error.error.class, FailureClass::Answered);
        assert_eq!(error.error.code.to_string(), "42P01");
        assert_eq!(error.committed, 1);
        assert_eq!(error.outcome.rows_inserted, 3_000);
        assert_eq!(count(&mut conn, "vehicle_signal").await, 3_000);
        pool.close().await;
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_record_that_even_unprocessed_rejects_stops_the_write_at_that_record() {
    with_migrated(|db| async move {
        // A schema that refuses `database_rejected` rows: the defect case of
        // `Writer::write`. The caller drops the record at `committed` and
        // writes the rest.
        let mut conn = db.owner().await;
        execute(
            &mut conn,
            "ALTER TABLE unprocessed_record ADD CONSTRAINT no_rejected \
             CHECK (reason <> 'database_rejected')",
        )
        .await;
        let batch = [
            Message::new(vehicle_data(0, vec![(8, SignalValue::Int32(1))])),
            Message::new(vehicle_data(
                1,
                vec![(9, SignalValue::Text("a\0b".to_owned()))],
            )),
            Message::new(vehicle_data(2, vec![(8, SignalValue::Int32(3))])),
        ];
        let pool = db.pool();
        let mut writer = Writer::new(pool.clone());
        let records = queued(&batch);
        let error = writer.write(&records, &mut || {}).await.unwrap_err();
        assert_eq!(error.error.class, FailureClass::Data);
        assert_eq!(error.error.code.to_string(), "23514");
        assert_eq!(error.committed, 1);
        assert_eq!(count(&mut conn, "vehicle_signal").await, 1);
        let rest = writer
            .write(&records[error.committed + 1..], &mut || {})
            .await
            .unwrap();
        assert_eq!(rest.rows_inserted, 1);
        assert_eq!(count(&mut conn, "vehicle_signal").await, 2);
        pool.close().await;
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_failure_after_a_split_in_one_transaction_says_which_records_are_stored() {
    with_migrated(|db| async move {
        // A, P, and X fit in one transaction. P fails with a data error, so
        // the writer splits the transaction: A commits, P is stored as
        // rejected, and X fails because the alert table does not exist.
        let batch = [
            Message::new(vehicle_data(0, vec![(8, SignalValue::Int32(1))])),
            Message::new(vehicle_data(
                1,
                vec![(9, SignalValue::Text("a\0b".to_owned()))],
            )),
            Message::new(alerts(2, vec![alert("TestAlert", vec![1], None, None)])),
        ];
        let mut conn = db.owner().await;
        execute(&mut conn, "DROP TABLE vehicle_alert").await;
        let pool = db.pool();
        let mut writer = Writer::new(pool.clone());
        let records = queued(&batch);
        let error = writer.write(&records, &mut || {}).await.unwrap_err();
        assert_eq!(error.error.class, FailureClass::Answered);
        assert_eq!(error.error.code.to_string(), "42P01");
        assert_eq!(error.committed, 2);
        assert_eq!(count(&mut conn, "vehicle_signal").await, 1);
        assert_eq!(count(&mut conn, "unprocessed_record").await, 1);
        // A second write of the stored records stores nothing twice.
        let again = writer
            .write(&records[..error.committed], &mut || {})
            .await
            .unwrap();
        assert_eq!((again.rows_inserted, again.unprocessed_inserted), (0, 0));
        assert_eq!(count(&mut conn, "vehicle_signal").await, 1);
        assert_eq!(count(&mut conn, "unprocessed_record").await, 1);
        pool.close().await;
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn the_progress_hook_runs_before_every_transaction_attempt() {
    with_migrated(|db| async move {
        // Four records that the database rejects: the split makes 2N - 1 = 7
        // failed attempts, and each record needs one reject insert.
        let batch: Vec<Message> = (0..4)
            .map(|index| {
                Message::new(vehicle_data(
                    index,
                    vec![(9, SignalValue::Text("a\0b".to_owned()))],
                ))
                .with_payload(format!("record {index}").as_bytes())
            })
            .collect();
        let pool = db.pool();
        let mut writer = Writer::new(pool.clone());
        let mut beats = 0_u64;
        let outcome = writer
            .write(&queued(&batch), &mut || beats += 1)
            .await
            .unwrap();
        assert_eq!(outcome.rejected_records, 4);
        assert_eq!((outcome.attempts, outcome.transactions), (11, 4));
        assert_eq!(beats, outcome.attempts);
        pool.close().await;
    })
    .await;
}
