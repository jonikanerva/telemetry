//! Tests of the row building without a database. Every VIN and value is
//! invented.

use std::collections::{BTreeMap, HashMap};

use super::*;
use crate::record::{
    Alert, Alerts, Connectivity, Errors, JsonObject, Location, Signal, Unprocessed, VehicleData,
    VehicleError, Vin,
};

const VIN: &str = "TESTVIN0000000001";

fn at(second: i64, nanosecond: i32) -> Timestamp {
    Timestamp::new(second, nanosecond).unwrap()
}

fn vehicles() -> HashMap<String, VehicleId> {
    HashMap::from([(VIN.to_owned(), 7)])
}

fn data(created: i64, signals: Vec<Signal>) -> Decoded {
    Decoded::VehicleData(VehicleData {
        vin: Vin::new(VIN.to_owned()),
        created_at: at(created, 0),
        received_at: at(created + 1, 123_456_789),
        is_resend: false,
        signals,
        unknown_value_variants: 0,
        unknown_fields: false,
    })
}

fn signal(field: i32, value: SignalValue) -> Signal {
    Signal { field, value }
}

fn queued(decoded: &Decoded) -> QueuedRecord<'_> {
    QueuedRecord {
        topic: b"tesla_telemetry_V",
        payload: b"\x0a\x00",
        decoded,
    }
}

fn build(decoded: &[Decoded]) -> Plan {
    let records: Vec<_> = decoded.iter().map(queued).collect();
    Plan::build(&records, &vehicles()).unwrap()
}

#[test]
fn reasons_match_the_decode_and_adr_0002() {
    assert_eq!(
        UNPROCESSED_REASONS,
        [
            "decode_failed",
            "unknown_topic",
            "missing_created_at",
            "unknown_value_variant",
            "unknown_fields",
            "conflicting_duplicate",
            "database_rejected",
        ]
    );
    // A new decode reason must be added to UNPROCESSED_REASONS (and to the
    // CHECK constraint with a migration). The match has no wildcard arm,
    // so a new variant stops this test from compiling.
    for reason in [
        UnprocessedReason::DecodeFailed,
        UnprocessedReason::UnknownTopic,
        UnprocessedReason::MissingCreatedAt,
        UnprocessedReason::UnknownValueVariant,
        UnprocessedReason::UnknownFields,
    ] {
        match reason {
            UnprocessedReason::DecodeFailed
            | UnprocessedReason::UnknownTopic
            | UnprocessedReason::MissingCreatedAt
            | UnprocessedReason::UnknownValueVariant
            | UnprocessedReason::UnknownFields => {
                assert!(UNPROCESSED_REASONS.contains(&reason.as_str()));
            }
        }
    }
}

#[test]
fn floor_micros_never_moves_forward() {
    assert_eq!(floor_micros(at(10, 123_456_789)), at(10, 123_456_000));
    assert_eq!(floor_micros(at(10, 999)), at(10, 0));
    assert_eq!(floor_micros(at(10, 1_000)), at(10, 1_000));
    // Before 1970 the nanoseconds of jiff are negative; the floor still
    // goes toward the past.
    assert_eq!(floor_micros(at(-10, -1)), at(-10, -1_000));
    assert_eq!(floor_micros(at(-10, -1_000)), at(-10, -1_000));
    assert_eq!(floor_micros(Timestamp::MIN), Timestamp::MIN);
    assert_eq!(
        floor_micros(Timestamp::MAX),
        at(Timestamp::MAX.as_second(), 999_999_000)
    );
}

#[test]
fn received_at_is_truncated_to_microseconds() {
    let plan = build(&[data(100, vec![signal(8, SignalValue::Int32(1))])]);
    assert_eq!(plan.signals[0].received_at, at(101, 123_456_000));
}

#[test]
fn every_variant_fills_one_column_group() {
    let mut object = JsonObject::new();
    object.insert("hour".to_owned(), 7.into());
    let cases = [
        (SignalValue::Text("12.3".to_owned()), 1),
        (SignalValue::Int32(-5), 2),
        (SignalValue::Int64(i64::MAX), 3),
        (SignalValue::Float32(1.5), 4),
        (SignalValue::Float64(-2.25), 5),
        (SignalValue::Bool(false), 6),
        (
            SignalValue::Location(Location {
                latitude: 12.5,
                longitude: -45.25,
            }),
            7,
        ),
        (SignalValue::Invalid(true), 10),
        (SignalValue::Time(object), 31),
    ];
    for (value, value_type) in cases {
        let columns = SignalValueColumns::from_value(&value);
        assert_eq!(columns.value_type, value_type);
        let filled = [
            columns.int_value.is_some(),
            columns.float_bits.is_some(),
            columns.latitude_bits.is_some(),
            columns.bool_value.is_some(),
            columns.text_value.is_some(),
            columns.json_value.is_some(),
        ];
        assert_eq!(filled.iter().filter(|&&set| set).count(), 1, "{value:?}");
        assert_eq!(
            columns.latitude_bits.is_some(),
            columns.longitude_bits.is_some()
        );
    }
    let int32 = SignalValueColumns::from_value(&SignalValue::Int32(5));
    let int64 = SignalValueColumns::from_value(&SignalValue::Int64(5));
    assert!(int32 != int64, "the variant is part of the value");
    let float32 = SignalValueColumns::from_value(&SignalValue::Float32(0.1));
    assert_eq!(float32.float_bits, Some(f64::from(0.1_f32).to_bits()));
}

#[test]
fn nan_equals_the_same_nan_and_signed_zeros_differ() {
    let columns = |number: f64| SignalValueColumns::from_value(&SignalValue::Float64(number));
    assert!(columns(f64::NAN) == columns(f64::NAN));
    let other_nan = f64::from_bits(f64::NAN.to_bits() | 1);
    assert!(other_nan.is_nan());
    assert!(columns(f64::NAN) != columns(other_nan));
    assert!(columns(0.0) != columns(-0.0));
    assert!(columns(1.0) == columns(1.0));
}

#[test]
fn an_equal_copy_in_the_batch_is_a_duplicate() {
    let first = data(100, vec![signal(8, SignalValue::Int32(1))]);
    let resend = data(100, vec![signal(8, SignalValue::Int32(1))]);
    let plan = build(&[first, resend]);
    assert_eq!(plan.signals.len(), 1);
    assert_eq!(plan.signals[0].record, 0, "the first copy wins");
    assert_eq!(plan.duplicates, 1);
    assert!(plan.conflicting.is_empty());
}

#[test]
fn a_different_copy_in_the_batch_makes_the_later_record_a_conflict() {
    let first = data(
        100,
        vec![
            signal(8, SignalValue::Int32(1)),
            signal(9, SignalValue::Bool(true)),
        ],
    );
    let later = data(
        100,
        vec![
            signal(8, SignalValue::Int32(2)),
            signal(10, SignalValue::Bool(true)),
        ],
    );
    let plan = build(&[first, later]);
    assert_eq!(plan.conflicting, BTreeSet::from([1]));
    let stored: Vec<_> = plan
        .signals
        .iter()
        .map(|row| (row.key.field, row.record))
        .collect();
    // The other datum of the later record is stored; the conflicting one is not.
    assert_eq!(stored, vec![(8, 0), (9, 0), (10, 1)]);
    let rows = plan.unprocessed_rows(&plan.conflicting);
    assert_eq!(
        rows,
        vec![UnprocessedRow {
            record: 1,
            reason: Reason::ConflictingDuplicate,
            sqlstate: None,
        }]
    );
}

#[test]
fn two_datums_of_one_record_with_one_key() {
    let same = data(
        100,
        vec![
            signal(8, SignalValue::Int32(1)),
            signal(8, SignalValue::Int32(1)),
        ],
    );
    let plan = build(&[same]);
    assert_eq!((plan.signals.len(), plan.duplicates), (1, 1));
    let other = data(
        100,
        vec![
            signal(8, SignalValue::Int32(1)),
            signal(8, SignalValue::Int32(2)),
        ],
    );
    let plan = build(&[other]);
    assert_eq!(plan.signals.len(), 1);
    assert_eq!(plan.conflicting, BTreeSet::from([0]));
}

#[test]
fn rows_are_in_key_order() {
    let plan = build(&[
        data(
            200,
            vec![
                signal(9, SignalValue::Int32(1)),
                signal(3, SignalValue::Int32(1)),
            ],
        ),
        data(100, vec![signal(9, SignalValue::Int32(1))]),
    ]);
    let keys: Vec<_> = plan
        .signals
        .iter()
        .map(|row| (row.key.field, row.key.created_at.as_second()))
        .collect();
    assert_eq!(keys, vec![(3, 200), (9, 100), (9, 200)]);
}

#[test]
fn decode_reasons_become_unprocessed_rows_and_keep_the_rows() {
    let partial = Decoded::VehicleData(VehicleData {
        vin: Vin::new(VIN.to_owned()),
        created_at: at(100, 0),
        received_at: at(101, 0),
        is_resend: true,
        signals: vec![signal(8, SignalValue::Int32(1))],
        unknown_value_variants: 2,
        unknown_fields: false,
    });
    let whole = Decoded::Unprocessed(Unprocessed {
        reason: UnprocessedReason::UnknownTopic,
        received_at: at(101, 0),
    });
    let plan = build(&[partial, whole]);
    assert_eq!(plan.signals.len(), 1);
    assert!(plan.signals[0].is_resend);
    assert_eq!(
        plan.unprocessed
            .iter()
            .map(|row| (row.record, row.reason.as_str()))
            .collect::<Vec<_>>(),
        vec![(0, "unknown_value_variant"), (1, "unknown_topic")]
    );
}

#[test]
fn records_without_rows_need_no_vehicle() {
    let empty = data(100, Vec::new());
    let unprocessed = Decoded::Unprocessed(Unprocessed {
        reason: UnprocessedReason::DecodeFailed,
        received_at: at(101, 0),
    });
    let records = [queued(&empty), queued(&unprocessed)];
    assert_eq!(vins_with_rows(&records), Vec::new());
    assert!(Plan::build(&records, &HashMap::new()).is_ok());
    let with_rows = data(100, vec![signal(1, SignalValue::Int32(1))]);
    assert_eq!(
        Plan::build(&[queued(&with_rows)], &HashMap::new()).err(),
        Some(MissingVehicle)
    );
}

#[test]
fn vins_are_listed_once_with_the_first_receive_time() {
    let first = data(100, vec![signal(1, SignalValue::Int32(1))]);
    let second = data(200, vec![signal(1, SignalValue::Int32(1))]);
    let records = [queued(&first), queued(&second)];
    assert_eq!(vins_with_rows(&records), vec![(VIN, at(101, 123_456_000))]);
}

#[test]
fn alerts_errors_and_connectivity_become_rows() {
    let alerts = Decoded::Alerts(Alerts {
        vin: Vin::new(VIN.to_owned()),
        created_at: at(100, 0),
        received_at: at(101, 0),
        alerts: vec![
            Alert {
                name: "TestAlert_a001".to_owned(),
                audiences: vec![1, 2],
                started_at: Some(at(90, 0)),
                ended_at: None,
            },
            Alert {
                name: "TestAlert_a002".to_owned(),
                audiences: Vec::new(),
                started_at: None,
                ended_at: None,
            },
        ],
        unknown_fields: false,
    });
    let errors = Decoded::Errors(Errors {
        vin: Vin::new(VIN.to_owned()),
        created_at: at(100, 0),
        received_at: at(101, 0),
        errors: vec![VehicleError {
            created_at: None,
            name: "test_error".to_owned(),
            tags: BTreeMap::from([("key".to_owned(), "value".to_owned())]),
            body: "invented body".to_owned(),
        }],
        unknown_fields: true,
    });
    let connectivity = Decoded::Connectivity(Connectivity {
        vin: Vin::new(VIN.to_owned()),
        connection_id: Uuid::from_u128(1),
        status: 1,
        network_interface: String::new(),
        created_at: at(100, 0),
        received_at: at(101, 0),
        unknown_fields: false,
    });
    let plan = build(&[alerts, errors, connectivity]);
    assert_eq!(
        plan.alerts
            .iter()
            .map(|row| row.audiences.as_str())
            .collect::<Vec<_>>(),
        vec!["{1,2}", "{}"]
    );
    assert_eq!(plan.errors[0].tags, serde_json::json!({ "key": "value" }));
    assert_eq!(plan.errors[0].vehicle_id, 7);
    assert_eq!(plan.connectivity[0].value.vehicle_id, 7);
    assert_eq!(plan.connectivity[0].value.network_interface, "");
    assert_eq!(
        plan.unprocessed
            .iter()
            .map(|row| (row.record, row.reason.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "unknown_fields")]
    );
}

#[test]
fn stored_rows_are_compared_by_the_same_rule() {
    let one = SignalValueColumns::from_value(&SignalValue::Int32(1));
    let two = SignalValueColumns::from_value(&SignalValue::Int32(2));
    let key = |field| SignalKey {
        vehicle_id: 1,
        field,
        created_at: at(1, 0),
    };
    let stored = HashMap::from([(key(1), one.clone()), (key(2), one.clone())]);
    let candidates = [(key(1), &one, 0), (key(2), &two, 1), (key(3), &one, 2)];
    let (duplicates, conflicting) = compare_stored(&candidates, &stored);
    assert_eq!(duplicates, 1);
    // A key that the insert skipped but the read did not find is kept as
    // a conflict.
    assert_eq!(conflicting, BTreeSet::from([1, 2]));
}

#[test]
fn queued_record_debug_shows_sizes_only() {
    let decoded = data(100, vec![signal(1, SignalValue::Text("secret".to_owned()))]);
    let record = QueuedRecord {
        topic: b"tesla_telemetry_V",
        payload: b"TESTVIN0000000001",
        decoded: &decoded,
    };
    let text = format!("{record:?}");
    assert_eq!(
        text,
        "QueuedRecord { topic_bytes: 17, payload_bytes: 17, decoded: \"VehicleData\" }"
    );
}
