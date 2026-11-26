//! Tests of the decode.
//!
//! The payloads are built with the types that `build.rs` generates from the
//! vendored schema, or byte by byte where a test needs bytes that those
//! types cannot make. Every VIN, coordinate, and signal value is invented.
//! The test VIN contains the letter `I`, which a real VIN never contains.
//!
//! The expected numbers come from the schema itself: the tests read the
//! descriptors of `proto/vehicle_data.proto` with `protox` and compare the
//! hand-written numbers of the decode with them.
//!
//! A failed property test writes its case to
//! `crates/ingest/proptest-regressions/`. Commit that file together with
//! the fix, as `proptest` recommends: every later test run then tries the
//! case first, so the failure cannot return unseen. The cases hold
//! generated values only, never vehicle data.

use std::collections::BTreeMap;

use jiff::Timestamp;
use prost::Message;
use prost::encoding::{self, WireType};
use prost_types::field_descriptor_proto::Type;
use prost_types::{DescriptorProto, FieldDescriptorProto, FileDescriptorSet};
use serde_json::json;

use super::proto::vehicle_alerts::{VehicleAlert, VehicleAlerts};
use super::proto::vehicle_connectivity::VehicleConnectivity;
use super::proto::vehicle_data::{
    self, Datum, Doors, Field, LocationValue, Payload, Time, TireLocation, value,
};
use super::proto::vehicle_error::{VehicleError as ProtoVehicleError, VehicleErrors};
use super::*;
use crate::record::VehicleError;

const TOPIC_V: &[u8] = b"tesla_telemetry_V";
const TOPIC_ALERTS: &[u8] = b"tesla_telemetry_alerts";
const TOPIC_ERRORS: &[u8] = b"tesla_telemetry_errors";
const TOPIC_CONNECTIVITY: &[u8] = b"tesla_telemetry_connectivity";

/// Not a real VIN: a VIN never contains `I`.
const TEST_VIN: &str = "TESTVIN0000000001";
const CONNECTION_ID: &str = "0b3c5e1a-7d2f-4c8e-9a6b-123456789abc";

fn received_at() -> Timestamp {
    Timestamp::new(1_800_000_000, 987_654_321).unwrap()
}

fn proto_time(seconds: i64, nanos: i32) -> prost_types::Timestamp {
    prost_types::Timestamp { seconds, nanos }
}

fn datum(field: Field, value: value::Value) -> Datum {
    Datum {
        key: field as i32,
        value: Some(vehicle_data::Value { value: Some(value) }),
    }
}

fn payload(data: Vec<Datum>) -> Payload {
    Payload {
        data,
        created_at: Some(proto_time(1_799_999_000, 123_456_789)),
        vin: TEST_VIN.to_owned(),
        is_resend: true,
    }
}

fn decode_v(message: &Payload) -> Decoded {
    decode(TOPIC_V, &message.encode_to_vec(), received_at())
}

fn vehicle_data(decoded: Decoded) -> VehicleData {
    match decoded {
        Decoded::VehicleData(record) => record,
        other => panic!("expected vehicle data, got {other:?}"),
    }
}

fn reason(decoded: &Decoded) -> Option<UnprocessedReason> {
    match decoded {
        Decoded::Unprocessed(Unprocessed {
            reason,
            received_at: at,
        }) => {
            assert_eq!(*at, received_at());
            Some(*reason)
        }
        _ => None,
    }
}

/// Decodes one datum and returns its signal value.
fn single_value(value: value::Value) -> SignalValue {
    let record = vehicle_data(decode_v(&payload(vec![datum(Field::Soc, value)])));
    assert_eq!(record.unknown_value_variants, 0);
    assert_eq!(record.signals.len(), 1);
    record.signals.into_iter().next().unwrap().value
}

/// The descriptors of the vendored `vehicle_data.proto`, parsed again at
/// test time, independent of the generated Rust types.
fn schema() -> FileDescriptorSet {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/proto");
    protox::compile([format!("{dir}/vehicle_data.proto")], [dir]).unwrap()
}

fn message<'a>(schema: &'a FileDescriptorSet, name: &str) -> &'a DescriptorProto {
    schema
        .file
        .iter()
        .flat_map(|file| &file.message_type)
        .find(|message| message.name() == name)
        .unwrap()
}

// --- Topics -----------------------------------------------------------

#[test]
fn an_unknown_topic_is_unprocessed() {
    let valid = payload(vec![datum(Field::Soc, value::Value::DoubleValue(50.5))]).encode_to_vec();
    // `record::tests` covers the topic rules. Here: the decode stops at
    // the topic, also for a valid payload.
    for topic in [
        &b""[..],
        b"tesla_telemetry_metrics",
        b"other_namespace_V",
        b"\xff\xfe_V",
    ] {
        let decoded = decode(topic, &valid, received_at());
        assert_eq!(
            reason(&decoded),
            Some(UnprocessedReason::UnknownTopic),
            "topic {topic:?}"
        );
        assert_eq!(
            decoded.unprocessed_reason(),
            Some(UnprocessedReason::UnknownTopic)
        );
    }
}

// --- Vehicle data ------------------------------------------------------

#[test]
fn decodes_a_vehicle_data_record() {
    let message = payload(vec![
        datum(Field::VehicleSpeed, value::Value::FloatValue(42.5)),
        datum(
            Field::Location,
            value::Value::LocationValue(LocationValue {
                latitude: 12.5,
                longitude: -45.25,
            }),
        ),
        datum(Field::Gear, value::Value::ShiftStateValue(5)),
        datum(Field::Soc, value::Value::Invalid(true)),
    ]);
    let record = vehicle_data(decode_v(&message));
    assert_eq!(record.vin.expose(), TEST_VIN);
    assert_eq!(
        record.created_at,
        Timestamp::new(1_799_999_000, 123_456_000).unwrap()
    );
    assert_eq!(record.received_at, received_at());
    assert!(record.is_resend);
    assert_eq!(record.unknown_value_variants, 0);
    assert_eq!(
        record.signals,
        [
            Signal {
                field: Field::VehicleSpeed as i32,
                value: SignalValue::Float32(42.5),
            },
            Signal {
                field: Field::Location as i32,
                value: SignalValue::Location(Location {
                    latitude: 12.5,
                    longitude: -45.25,
                }),
            },
            Signal {
                field: Field::Gear as i32,
                value: SignalValue::Enum(EnumValue::new(9, 5)),
            },
            Signal {
                field: Field::Soc as i32,
                value: SignalValue::Invalid(true),
            },
        ]
    );
    let decoded = decode_v(&message);
    assert_eq!(decoded.unprocessed_reason(), None);
}

#[test]
fn keeps_the_scalar_variants_and_their_widths() {
    assert_eq!(
        single_value(value::Value::StringValue("12.3".to_owned())),
        SignalValue::Text("12.3".to_owned())
    );
    assert_eq!(
        single_value(value::Value::IntValue(i32::MIN)),
        SignalValue::Int32(i32::MIN)
    );
    // Above 2^53: a cast through `f64` would change it.
    assert_eq!(
        single_value(value::Value::LongValue(9_007_199_254_740_993)),
        SignalValue::Int64(9_007_199_254_740_993)
    );
    assert_eq!(
        single_value(value::Value::FloatValue(0.1)),
        SignalValue::Float32(0.1)
    );
    assert_eq!(
        single_value(value::Value::DoubleValue(-0.1)),
        SignalValue::Float64(-0.1)
    );
    assert_eq!(
        single_value(value::Value::BooleanValue(false)),
        SignalValue::Bool(false)
    );
}

#[test]
fn keeps_the_invalid_marker_apart_from_false_and_zero() {
    let marker = single_value(value::Value::Invalid(true));
    assert_eq!(marker, SignalValue::Invalid(true));
    assert_eq!(marker.value_type(), 10);
    assert_ne!(marker, SignalValue::Bool(true));
    assert_eq!(
        single_value(value::Value::Invalid(false)),
        SignalValue::Invalid(false)
    );
}

#[test]
fn keeps_float_values_that_are_not_finite() {
    let SignalValue::Float64(nan) = single_value(value::Value::DoubleValue(f64::NAN)) else {
        panic!("expected a double");
    };
    assert!(nan.is_nan());
    assert_eq!(
        single_value(value::Value::FloatValue(f32::NEG_INFINITY)),
        SignalValue::Float32(f32::NEG_INFINITY)
    );
}

#[test]
fn keeps_enum_numbers_also_unknown_ones() {
    for number in [0, 3, 999, -7, i32::MAX] {
        assert_eq!(
            single_value(value::Value::ChargePortLatchValue(number)),
            SignalValue::Enum(EnumValue::new(19, number))
        );
    }
}

#[test]
fn keeps_an_unknown_field_number() {
    for key in [0, 9_999, -1] {
        let message = payload(vec![Datum {
            key,
            value: Some(vehicle_data::Value {
                value: Some(value::Value::IntValue(7)),
            }),
        }]);
        let record = vehicle_data(decode_v(&message));
        assert_eq!(record.signals[0].field, key);
    }
}

#[test]
fn makes_json_objects_with_every_proto_field_name() {
    assert_eq!(
        single_value(value::Value::DoorValue(Doors {
            driver_front: true,
            ..Doors::default()
        })),
        SignalValue::Doors(
            json!({
                "DriverFront": true,
                "DriverRear": false,
                "PassengerFront": false,
                "PassengerRear": false,
                "TrunkFront": false,
                "TrunkRear": false,
            })
            .as_object()
            .unwrap()
            .clone()
        )
    );
    assert_eq!(
        single_value(value::Value::TimeValue(Time {
            hour: 7,
            minute: 0,
            second: 30,
        })),
        SignalValue::Time(
            json!({"hour": 7, "minute": 0, "second": 30})
                .as_object()
                .unwrap()
                .clone()
        )
    );
    let SignalValue::TireLocation(tires) =
        single_value(value::Value::TireLocationValue(TireLocation::default()))
    else {
        panic!("expected a tire location");
    };
    assert_eq!(tires.len(), 10);
    assert!(tires.values().all(|value| value == &json!(false)));
}

/// Every variant of the `Value` oneof in the schema: the decode gives the
/// field number of the schema as `value_type`, and a value of the matching
/// kind. The bytes carry the field number of the descriptor, so this test
/// does not depend on the hand-written numbers in the decode.
#[test]
fn value_type_is_the_schema_field_number_of_every_variant() {
    let schema = schema();
    let value_message = message(&schema, "Value");
    let variants: Vec<_> = value_message
        .field
        .iter()
        .filter(|field| field.oneof_index.is_some())
        .collect();
    assert_eq!(variants.len(), 56, "variants in the vendored schema");
    for field in variants {
        let number = u32::try_from(field.number()).unwrap();
        let mut bytes = Vec::new();
        match field.r#type() {
            Type::String => encoding::string::encode(number, &"x".to_owned(), &mut bytes),
            Type::Int32 => encoding::int32::encode(number, &-3, &mut bytes),
            Type::Int64 => encoding::int64::encode(number, &-3, &mut bytes),
            Type::Float => encoding::float::encode(number, &1.5, &mut bytes),
            Type::Double => encoding::double::encode(number, &1.5, &mut bytes),
            Type::Bool => encoding::bool::encode(number, &true, &mut bytes),
            Type::Enum => encoding::int32::encode(number, &2, &mut bytes),
            Type::Message => {
                encoding::encode_key(number, WireType::LengthDelimited, &mut bytes);
                encoding::encode_varint(0, &mut bytes);
            }
            other => panic!("variant {} has type {other:?}", field.name()),
        }
        let value = vehicle_data::Value::decode(bytes.as_slice()).unwrap();
        let signal = single_value(value.value.unwrap());
        assert_eq!(
            i32::from(signal.value_type()),
            field.number(),
            "variant {}",
            field.name()
        );
        let kind_matches = match (&signal, field.r#type()) {
            (SignalValue::Text(text), Type::String) => text == "x",
            (SignalValue::Int32(-3), Type::Int32)
            | (SignalValue::Int64(-3), Type::Int64)
            | (SignalValue::Bool(true) | SignalValue::Invalid(true), Type::Bool) => true,
            (SignalValue::Float32(number), Type::Float) => number.to_bits() == 1.5_f32.to_bits(),
            (SignalValue::Float64(number), Type::Double) => number.to_bits() == 1.5_f64.to_bits(),
            (SignalValue::Enum(value), Type::Enum) => value.number() == 2,
            (SignalValue::Location(_), Type::Message) => {
                field.type_name() == ".telemetry.vehicle_data.LocationValue"
            }
            (SignalValue::Doors(_), Type::Message) => {
                field.type_name() == ".telemetry.vehicle_data.Doors"
            }
            (SignalValue::Time(_), Type::Message) => {
                field.type_name() == ".telemetry.vehicle_data.Time"
            }
            (SignalValue::TireLocation(_), Type::Message) => {
                field.type_name() == ".telemetry.vehicle_data.TireLocation"
            }
            _ => false,
        };
        assert!(kind_matches, "variant {} gave {signal:?}", field.name());
        assert_eq!(
            matches!(signal, SignalValue::Invalid(_)),
            field.name() == "invalid"
        );
    }
}

/// Each JSON key holds the value of the schema field with that name: the
/// test sets one field at a time, by its number in the descriptor.
#[test]
fn json_keys_are_the_schema_field_names() {
    let schema = schema();
    for (message_name, variant_number) in [("Doors", 21), ("Time", 31), ("TireLocation", 39)] {
        let fields = &message(&schema, message_name).field;
        for field in fields {
            let number = u32::try_from(field.number()).unwrap();
            let mut inner = Vec::new();
            match field.r#type() {
                Type::Bool => encoding::bool::encode(number, &true, &mut inner),
                Type::Int32 => encoding::int32::encode(number, &41, &mut inner),
                other => panic!("{message_name}.{} has type {other:?}", field.name()),
            }
            let mut bytes = Vec::new();
            encoding::bytes::encode(variant_number, &inner, &mut bytes);
            let value = vehicle_data::Value::decode(bytes.as_slice()).unwrap();
            let object = match single_value(value.value.unwrap()) {
                SignalValue::Doors(object)
                | SignalValue::Time(object)
                | SignalValue::TireLocation(object) => object,
                other => panic!("{message_name} gave {other:?}"),
            };
            let names: Vec<_> = fields.iter().map(FieldDescriptorProto::name).collect();
            assert_eq!(object.len(), names.len());
            for (key, value) in &object {
                assert!(names.contains(&key.as_str()), "{message_name}: key {key}");
                let expected = if key == field.name() {
                    match field.r#type() {
                        Type::Bool => json!(true),
                        _ => json!(41),
                    }
                } else {
                    match field.r#type() {
                        Type::Bool => json!(false),
                        _ => json!(0),
                    }
                };
                assert_eq!(value, &expected, "{message_name}.{key}");
            }
        }
    }
}

// --- Upstream value changes (ADR 0001, "Wire format: protobuf") --------

/// Upstream turns a `Location` text into a `location_value` only when the
/// text parses. A text that it could not parse arrives as text and stays
/// text: the decode parses nothing.
#[test]
fn a_location_text_stays_text() {
    let message = payload(vec![datum(
        Field::Location,
        value::Value::StringValue("(12.500000 N, 45.250000 W)".to_owned()),
    )]);
    let record = vehicle_data(decode_v(&message));
    assert_eq!(
        record.signals[0].value,
        SignalValue::Text("(12.500000 N, 45.250000 W)".to_owned())
    );
}

/// Upstream rewrites a text in scientific notation to five decimals. The
/// decode stores the rewritten text as it arrives, and does not rewrite a
/// text itself.
#[test]
fn a_number_text_stays_text() {
    for text in ["0.00100", "1e-3", "  7 ", ""] {
        assert_eq!(
            single_value(value::Value::StringValue(text.to_owned())),
            SignalValue::Text(text.to_owned())
        );
    }
}

/// Upstream shifts the AEB enum of client 1.3.0. The decode stores the
/// number that arrives and does not shift it again.
#[test]
fn the_aeb_number_is_stored_as_it_arrives() {
    for number in 0..=5 {
        let message = payload(vec![datum(
            Field::AutomaticEmergencyBrakingState,
            value::Value::AutomaticEmergencyBrakingStateValue(number),
        )]);
        let record = vehicle_data(decode_v(&message));
        assert_eq!(
            record.signals[0].value,
            SignalValue::Enum(EnumValue::new(60, number))
        );
    }
}

/// Upstream converts an alert time with seconds in milliseconds before the
/// dispatch. A time that still has such seconds is out of range, and the
/// decode does not guess a unit for it.
#[test]
fn an_alert_time_in_milliseconds_is_not_converted() {
    let message = VehicleAlerts {
        alerts: vec![VehicleAlert {
            name: "TestAlert".to_owned(),
            audiences: vec![1],
            started_at: Some(proto_time(315_576_000_000, 0)),
            ended_at: None,
        }],
        created_at: Some(proto_time(1_799_999_000, 0)),
        vin: TEST_VIN.to_owned(),
    };
    let decoded = decode(TOPIC_ALERTS, &message.encode_to_vec(), received_at());
    assert_eq!(reason(&decoded), Some(UnprocessedReason::DecodeFailed));
}

// --- Unknown value variants (partial success) --------------------------

/// The bytes of a `Datum` whose `Value` has the field `number`, which the
/// schema does not have.
fn datum_with_unknown_variant(field: Field, number: u32) -> Vec<u8> {
    let mut value = Vec::new();
    encoding::int32::encode(number, &1, &mut value);
    let mut datum = Vec::new();
    encoding::int32::encode(1, &(field as i32), &mut datum);
    encoding::bytes::encode(2, &value, &mut datum);
    datum
}

#[test]
fn a_datum_without_a_known_variant_is_counted_and_the_others_are_kept() {
    let known = payload(vec![
        datum(Field::Soc, value::Value::DoubleValue(50.5)),
        datum(Field::Gear, value::Value::ShiftStateValue(1)),
    ]);
    // A repeated field can be appended: these bytes add three datums
    // without a known variant between and after the known ones.
    let mut bytes = Vec::new();
    encoding::bytes::encode(1, &known.data[0].encode_to_vec(), &mut bytes);
    encoding::bytes::encode(
        1,
        &datum_with_unknown_variant(Field::Odometer, 99),
        &mut bytes,
    );
    // A datum without a value, and a datum with an empty value.
    encoding::bytes::encode(
        1,
        &Datum {
            key: Field::PackVoltage as i32,
            value: None,
        }
        .encode_to_vec(),
        &mut bytes,
    );
    encoding::bytes::encode(
        1,
        &Datum {
            key: Field::PackCurrent as i32,
            value: Some(vehicle_data::Value { value: None }),
        }
        .encode_to_vec(),
        &mut bytes,
    );
    encoding::bytes::encode(1, &known.data[1].encode_to_vec(), &mut bytes);
    let rest = Payload {
        data: Vec::new(),
        ..known.clone()
    };
    bytes.extend(rest.encode_to_vec());

    let decoded = decode(TOPIC_V, &bytes, received_at());
    assert_eq!(
        decoded.unprocessed_reason(),
        Some(UnprocessedReason::UnknownValueVariant)
    );
    let record = vehicle_data(decoded);
    assert_eq!(record.unknown_value_variants, 3);
    assert_eq!(
        record.signals,
        [
            Signal {
                field: Field::Soc as i32,
                value: SignalValue::Float64(50.5),
            },
            Signal {
                field: Field::Gear as i32,
                value: SignalValue::Enum(EnumValue::new(9, 1)),
            },
        ]
    );
    assert_eq!(record.vin.expose(), TEST_VIN);
}

#[test]
fn a_record_with_only_unknown_variants_still_has_its_reason() {
    let mut bytes = Vec::new();
    encoding::bytes::encode(1, &datum_with_unknown_variant(Field::Soc, 61), &mut bytes);
    bytes.extend(payload(Vec::new()).encode_to_vec());
    let decoded = decode(TOPIC_V, &bytes, received_at());
    assert_eq!(
        decoded.unprocessed_reason(),
        Some(UnprocessedReason::UnknownValueVariant)
    );
    let record = vehicle_data(decoded);
    assert_eq!(record.signals, []);
    assert_eq!(record.unknown_value_variants, 1);
}

#[test]
fn a_record_without_datums_has_no_signals_and_no_reason() {
    let decoded = decode_v(&payload(Vec::new()));
    assert_eq!(decoded.unprocessed_reason(), None);
    assert_eq!(vehicle_data(decoded).signals, []);
}

// --- Time --------------------------------------------------------------

fn created_at_of(seconds: i64, nanos: i32) -> Decoded {
    let message = Payload {
        created_at: Some(proto_time(seconds, nanos)),
        ..payload(vec![datum(Field::Soc, value::Value::IntValue(1))])
    };
    decode_v(&message)
}

#[test]
fn truncates_created_at_to_microseconds() {
    for (nanos, expected) in [
        (0, 0),
        (999, 0),
        (1_000, 1_000),
        (123_456_789, 123_456_000),
        (999_999_999, 999_999_000),
    ] {
        let record = vehicle_data(created_at_of(1_799_999_000, nanos));
        assert_eq!(
            record.created_at,
            Timestamp::new(1_799_999_000, expected).unwrap(),
            "nanos {nanos}"
        );
    }
}

/// Before 1970 the nanoseconds still count forward from the seconds, so
/// the truncation moves the instant back, not toward 1970.
#[test]
fn truncation_before_1970_moves_back_in_time() {
    let record = vehicle_data(created_at_of(-1, 999_999_999));
    assert_eq!(record.created_at.as_nanosecond(), -1_000);
    let record = vehicle_data(created_at_of(-1, 1));
    assert_eq!(record.created_at.as_nanosecond(), -1_000_000_000);
}

#[test]
fn accepts_the_range_of_the_timestamp_type_that_jiff_holds() {
    let earliest = vehicle_data(created_at_of(MIN_TIMESTAMP_SECONDS, 0));
    assert_eq!(earliest.created_at.to_string(), "0001-01-01T00:00:00Z");
    // 9999-12-30T00:00:00Z
    let late = vehicle_data(created_at_of(253_402_128_000, 0));
    assert_eq!(late.created_at.to_string(), "9999-12-30T00:00:00Z");
}

#[test]
fn a_timestamp_outside_its_range_is_unprocessed() {
    for (seconds, nanos) in [
        (MIN_TIMESTAMP_SECONDS - 1, 999_999_999),
        // Valid for the protobuf type, but later than `jiff` can hold.
        (MAX_TIMESTAMP_SECONDS, 0),
        (MAX_TIMESTAMP_SECONDS + 1, 0),
        (i64::MIN, 0),
        (i64::MAX, 0),
        (1_799_999_000, -1),
        (1_799_999_000, 1_000_000_000),
        (1_799_999_000, i32::MIN),
    ] {
        assert_eq!(
            reason(&created_at_of(seconds, nanos)),
            Some(UnprocessedReason::DecodeFailed),
            "seconds {seconds} nanos {nanos}"
        );
    }
}

#[test]
fn a_vehicle_data_record_without_created_at_is_unprocessed() {
    let message = Payload {
        created_at: None,
        ..payload(vec![datum(Field::Soc, value::Value::IntValue(1))])
    };
    assert_eq!(
        reason(&decode_v(&message)),
        Some(UnprocessedReason::MissingCreatedAt)
    );
}

#[test]
fn the_receive_time_is_kept_unchanged() {
    let at = Timestamp::new(1_800_000_000, 1).unwrap();
    let message = payload(vec![datum(Field::Soc, value::Value::IntValue(1))]);
    let decoded = decode(TOPIC_V, &message.encode_to_vec(), at);
    assert_eq!(vehicle_data(decoded).received_at, at);
}

// --- Alerts ------------------------------------------------------------

#[test]
fn decodes_an_alerts_record() {
    let message = VehicleAlerts {
        alerts: vec![
            VehicleAlert {
                name: "TestAlertActive".to_owned(),
                audiences: vec![1, 2, 99],
                started_at: Some(proto_time(1_799_998_000, 500_000_999)),
                ended_at: None,
            },
            VehicleAlert {
                name: "TestAlertEnded".to_owned(),
                audiences: Vec::new(),
                started_at: Some(proto_time(1_799_997_000, 0)),
                ended_at: Some(proto_time(1_799_997_500, 0)),
            },
        ],
        created_at: Some(proto_time(1_799_999_000, 7_999)),
        vin: TEST_VIN.to_owned(),
    };
    let decoded = decode(TOPIC_ALERTS, &message.encode_to_vec(), received_at());
    assert_eq!(decoded.unprocessed_reason(), None);
    let Decoded::Alerts(record) = decoded else {
        panic!("expected alerts, got {decoded:?}");
    };
    assert_eq!(
        record,
        Alerts {
            vin: Vin::new(TEST_VIN.to_owned()),
            created_at: Timestamp::new(1_799_999_000, 7_000).unwrap(),
            received_at: received_at(),
            alerts: vec![
                Alert {
                    name: "TestAlertActive".to_owned(),
                    audiences: vec![1, 2, 99],
                    started_at: Some(Timestamp::new(1_799_998_000, 500_000_000).unwrap()),
                    ended_at: None,
                },
                Alert {
                    name: "TestAlertEnded".to_owned(),
                    audiences: Vec::new(),
                    started_at: Some(Timestamp::new(1_799_997_000, 0).unwrap()),
                    ended_at: Some(Timestamp::new(1_799_997_500, 0).unwrap()),
                },
            ],
            unknown_fields: false,
        }
    );
}

/// ADR 0002 sends a record without `created_at` to unprocessed as a
/// whole, for every record type ("Unknown data").
#[test]
fn an_alerts_or_errors_record_without_created_at_is_unprocessed() {
    let alerts = VehicleAlerts {
        alerts: vec![VehicleAlert {
            name: "TestAlert".to_owned(),
            audiences: vec![1],
            started_at: Some(proto_time(1_799_998_000, 0)),
            ended_at: None,
        }],
        created_at: None,
        vin: TEST_VIN.to_owned(),
    };
    let decoded = decode(TOPIC_ALERTS, &alerts.encode_to_vec(), received_at());
    assert_eq!(reason(&decoded), Some(UnprocessedReason::MissingCreatedAt));

    let errors = VehicleErrors {
        errors: vec![ProtoVehicleError {
            created_at: Some(proto_time(1_799_998_000, 0)),
            name: "test_error".to_owned(),
            ..Default::default()
        }],
        created_at: None,
        vin: TEST_VIN.to_owned(),
    };
    let decoded = decode(TOPIC_ERRORS, &errors.encode_to_vec(), received_at());
    assert_eq!(reason(&decoded), Some(UnprocessedReason::MissingCreatedAt));
}

/// The time of an alert or error item can be missing. It stays `None`.
#[test]
fn a_missing_item_time_stays_none() {
    let alerts = VehicleAlerts {
        alerts: vec![VehicleAlert {
            name: "TestAlert".to_owned(),
            audiences: vec![1],
            started_at: None,
            ended_at: None,
        }],
        created_at: Some(proto_time(1_799_999_000, 0)),
        vin: TEST_VIN.to_owned(),
    };
    let decoded = decode(TOPIC_ALERTS, &alerts.encode_to_vec(), received_at());
    let Decoded::Alerts(record) = decoded else {
        panic!("expected alerts, got {decoded:?}");
    };
    assert_eq!(record.alerts[0].started_at, None);
    assert_eq!(record.alerts[0].ended_at, None);

    let errors = VehicleErrors {
        errors: vec![ProtoVehicleError {
            created_at: None,
            name: "test_error".to_owned(),
            ..Default::default()
        }],
        created_at: Some(proto_time(1_799_999_000, 0)),
        vin: TEST_VIN.to_owned(),
    };
    let decoded = decode(TOPIC_ERRORS, &errors.encode_to_vec(), received_at());
    let Decoded::Errors(record) = decoded else {
        panic!("expected errors, got {decoded:?}");
    };
    assert_eq!(record.errors[0].created_at, None);
}

// --- Errors ------------------------------------------------------------

#[test]
fn decodes_an_errors_record() {
    let message = VehicleErrors {
        errors: vec![ProtoVehicleError {
            created_at: Some(proto_time(1_799_998_000, 1_234_567)),
            name: "test_error".to_owned(),
            tags: BTreeMap::from([
                ("b".to_owned(), "2".to_owned()),
                ("a".to_owned(), String::new()),
            ]),
            body: "test body".to_owned(),
        }],
        created_at: Some(proto_time(1_799_999_000, 999)),
        vin: TEST_VIN.to_owned(),
    };
    let decoded = decode(TOPIC_ERRORS, &message.encode_to_vec(), received_at());
    assert_eq!(decoded.unprocessed_reason(), None);
    let Decoded::Errors(record) = decoded else {
        panic!("expected errors, got {decoded:?}");
    };
    assert_eq!(
        record,
        Errors {
            vin: Vin::new(TEST_VIN.to_owned()),
            created_at: Timestamp::new(1_799_999_000, 0).unwrap(),
            received_at: received_at(),
            errors: vec![VehicleError {
                created_at: Some(Timestamp::new(1_799_998_000, 1_234_000).unwrap()),
                name: "test_error".to_owned(),
                tags: BTreeMap::from([
                    ("a".to_owned(), String::new()),
                    ("b".to_owned(), "2".to_owned()),
                ]),
                body: "test body".to_owned(),
            }],
            unknown_fields: false,
        }
    );
}

#[test]
fn an_invalid_item_time_makes_the_whole_record_unprocessed() {
    let message = VehicleErrors {
        errors: vec![
            ProtoVehicleError {
                created_at: Some(proto_time(1_799_998_000, 0)),
                name: "valid".to_owned(),
                ..Default::default()
            },
            ProtoVehicleError {
                created_at: Some(proto_time(1_799_998_000, -5)),
                name: "invalid".to_owned(),
                ..Default::default()
            },
        ],
        created_at: Some(proto_time(1_799_999_000, 0)),
        vin: TEST_VIN.to_owned(),
    };
    let decoded = decode(TOPIC_ERRORS, &message.encode_to_vec(), received_at());
    assert_eq!(reason(&decoded), Some(UnprocessedReason::DecodeFailed));
}

// --- Connectivity ------------------------------------------------------

fn connectivity_message() -> VehicleConnectivity {
    VehicleConnectivity {
        vin: TEST_VIN.to_owned(),
        connection_id: CONNECTION_ID.to_owned(),
        status: 1,
        created_at: Some(proto_time(1_799_999_000, 999_999_999)),
        network_interface: String::new(),
    }
}

#[test]
fn decodes_a_connectivity_record() {
    let decoded = decode(
        TOPIC_CONNECTIVITY,
        &connectivity_message().encode_to_vec(),
        received_at(),
    );
    assert_eq!(decoded.unprocessed_reason(), None);
    assert_eq!(
        decoded,
        Decoded::Connectivity(Connectivity {
            vin: Vin::new(TEST_VIN.to_owned()),
            connection_id: Uuid::try_parse(CONNECTION_ID).unwrap(),
            status: 1,
            network_interface: String::new(),
            created_at: Timestamp::new(1_799_999_000, 999_999_000).unwrap(),
            received_at: received_at(),
            unknown_fields: false,
        })
    );
}

#[test]
fn keeps_an_unknown_connectivity_status() {
    let message = VehicleConnectivity {
        status: 7,
        network_interface: "wifi".to_owned(),
        ..connectivity_message()
    };
    let decoded = decode(TOPIC_CONNECTIVITY, &message.encode_to_vec(), received_at());
    let Decoded::Connectivity(record) = decoded else {
        panic!("expected connectivity, got {decoded:?}");
    };
    assert_eq!(record.status, 7);
    assert_eq!(record.network_interface, "wifi");
}

#[test]
fn a_connectivity_record_with_a_bad_connection_id_is_unprocessed() {
    for connection_id in ["", "not-a-uuid", "0b3c5e1a-7d2f-4c8e-9a6b-123456789ab"] {
        let message = VehicleConnectivity {
            connection_id: connection_id.to_owned(),
            ..connectivity_message()
        };
        let decoded = decode(TOPIC_CONNECTIVITY, &message.encode_to_vec(), received_at());
        assert_eq!(
            reason(&decoded),
            Some(UnprocessedReason::DecodeFailed),
            "{connection_id:?}"
        );
    }
}

#[test]
fn a_connectivity_record_without_created_at_is_unprocessed() {
    let message = VehicleConnectivity {
        created_at: None,
        ..connectivity_message()
    };
    let decoded = decode(TOPIC_CONNECTIVITY, &message.encode_to_vec(), received_at());
    assert_eq!(reason(&decoded), Some(UnprocessedReason::MissingCreatedAt));
}

// --- Malformed input ---------------------------------------------------

#[test]
fn bytes_that_are_not_protobuf_are_unprocessed() {
    for topic in [TOPIC_V, TOPIC_ALERTS, TOPIC_ERRORS, TOPIC_CONNECTIVITY] {
        for bytes in [
            &[0xff_u8; 12][..],
            // Field 1, length 100, but only 2 bytes follow.
            &[0x0a, 100, 1, 2],
            // Field 0 is not valid.
            &[0x00, 0x01],
            // A varint without an end.
            &[0x08, 0xff, 0xff, 0xff],
        ] {
            assert_eq!(
                reason(&decode(topic, bytes, received_at())),
                Some(UnprocessedReason::DecodeFailed),
                "topic {topic:?} bytes {bytes:?}"
            );
        }
    }
}

#[test]
fn text_that_is_not_utf8_is_unprocessed() {
    let mut bytes = Vec::new();
    let mut value = Vec::new();
    encoding::bytes::encode(1, &vec![0x66, 0xff, 0x66], &mut value);
    let mut datum = Vec::new();
    encoding::int32::encode(1, &(Field::Soc as i32), &mut datum);
    encoding::bytes::encode(2, &value, &mut datum);
    encoding::bytes::encode(1, &datum, &mut bytes);
    bytes.extend(payload(Vec::new()).encode_to_vec());
    assert_eq!(
        reason(&decode(TOPIC_V, &bytes, received_at())),
        Some(UnprocessedReason::DecodeFailed)
    );
}

#[test]
fn an_empty_payload_has_no_created_at() {
    for topic in [TOPIC_V, TOPIC_ALERTS, TOPIC_ERRORS, TOPIC_CONNECTIVITY] {
        assert_eq!(
            reason(&decode(topic, &[], received_at())),
            Some(UnprocessedReason::MissingCreatedAt),
            "topic {topic:?}"
        );
    }
}

/// Each cut of a valid record of each type either fails as a whole or
/// keeps a prefix of its items. The decode never invents an item from a
/// part of the bytes.
#[test]
fn a_truncated_payload_never_invents_items() {
    let vehicle_data = payload(vec![
        datum(Field::Soc, value::Value::DoubleValue(50.5)),
        datum(
            Field::Location,
            value::Value::LocationValue(LocationValue {
                latitude: 12.5,
                longitude: -45.25,
            }),
        ),
        datum(Field::Gear, value::Value::ShiftStateValue(5)),
        datum(
            Field::Odometer,
            value::Value::StringValue("1234.5".to_owned()),
        ),
    ]);
    let alerts = VehicleAlerts {
        alerts: vec![
            VehicleAlert {
                name: "TestAlertOne".to_owned(),
                audiences: vec![1, 2],
                started_at: Some(proto_time(1_799_998_000, 0)),
                ended_at: None,
            },
            VehicleAlert {
                name: "TestAlertTwo".to_owned(),
                audiences: vec![3],
                started_at: Some(proto_time(1_799_998_500, 0)),
                ended_at: Some(proto_time(1_799_998_600, 0)),
            },
        ],
        created_at: Some(proto_time(1_799_999_000, 0)),
        vin: TEST_VIN.to_owned(),
    };
    let errors = VehicleErrors {
        errors: vec![
            ProtoVehicleError {
                created_at: Some(proto_time(1_799_998_000, 0)),
                name: "test_error_one".to_owned(),
                tags: BTreeMap::from([("k".to_owned(), "v".to_owned())]),
                body: "body one".to_owned(),
            },
            ProtoVehicleError {
                created_at: None,
                name: "test_error_two".to_owned(),
                tags: BTreeMap::new(),
                body: "body two".to_owned(),
            },
        ],
        created_at: Some(proto_time(1_799_999_000, 0)),
        vin: TEST_VIN.to_owned(),
    };
    for (topic, bytes) in [
        (TOPIC_V, vehicle_data.encode_to_vec()),
        (TOPIC_ALERTS, alerts.encode_to_vec()),
        (TOPIC_ERRORS, errors.encode_to_vec()),
        (TOPIC_CONNECTIVITY, connectivity_message().encode_to_vec()),
    ] {
        let full = decode(topic, &bytes, received_at());
        assert_eq!(full.unprocessed_reason(), None, "topic {topic:?}");
        for end in 0..bytes.len() {
            let cut = decode(topic, &bytes[..end], received_at());
            let keeps_a_prefix = match (&cut, &full) {
                (Decoded::Unprocessed(_), _) => true,
                (Decoded::VehicleData(cut), Decoded::VehicleData(full)) => {
                    cut.unknown_value_variants == 0 && full.signals.starts_with(&cut.signals)
                }
                (Decoded::Alerts(cut), Decoded::Alerts(full)) => {
                    full.alerts.starts_with(&cut.alerts)
                }
                (Decoded::Errors(cut), Decoded::Errors(full)) => {
                    full.errors.starts_with(&cut.errors)
                }
                (Decoded::Connectivity(cut), Decoded::Connectivity(full)) => {
                    cut.connection_id == full.connection_id && cut.created_at == full.created_at
                }
                _ => false,
            };
            assert!(keeps_a_prefix, "topic {topic:?} cut at {end}: {cut:?}");
        }
    }
}

// --- Fields that the schema does not know ------------------------------

/// Appends a field with `number` and a varint value: a field that the
/// vendored schema does not have.
fn unknown_varint(number: u32, bytes: &mut Vec<u8>) {
    encoding::uint64::encode(number, &7, bytes);
}

/// Wraps `inner` as the length-delimited field `number`.
fn nested(number: u32, inner: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    encoding::bytes::encode(number, &inner.to_vec(), &mut bytes);
    bytes
}

fn soc_datum_bytes() -> Vec<u8> {
    datum(Field::Soc, value::Value::DoubleValue(50.5)).encode_to_vec()
}

/// A `V` payload with one datum, built from parts so that a test can add
/// a field at any depth.
fn v_payload(datum_bytes: &[u8], created_at: &[u8], extra: &[u8]) -> Vec<u8> {
    let mut bytes = nested(1, datum_bytes);
    bytes.extend(nested(2, created_at));
    encoding::string::encode(3, &TEST_VIN.to_owned(), &mut bytes);
    bytes.extend_from_slice(extra);
    bytes
}

fn created_at_bytes() -> Vec<u8> {
    proto_time(1_799_999_000, 0).encode_to_vec()
}

#[test]
fn an_unknown_field_at_any_depth_keeps_the_rows_and_reports_the_message() {
    let mut top_level = Vec::new();
    unknown_varint(5, &mut top_level);

    let mut datum_extra = soc_datum_bytes();
    unknown_varint(3, &mut datum_extra);

    // A known variant, then a variant that the schema does not have, in
    // the same `Value`: `prost` keeps the known one and skips the other.
    let mut value_bytes = vehicle_data::Value {
        value: Some(value::Value::DoubleValue(50.5)),
    }
    .encode_to_vec();
    unknown_varint(61, &mut value_bytes);
    let mut value_extra = Vec::new();
    encoding::int32::encode(1, &(Field::Soc as i32), &mut value_extra);
    value_extra.extend(nested(2, &value_bytes));

    let mut timestamp_extra = created_at_bytes();
    unknown_varint(3, &mut timestamp_extra);

    let mut doors = Doors::default().encode_to_vec();
    unknown_varint(7, &mut doors);
    let mut doors_datum = Vec::new();
    encoding::int32::encode(1, &(Field::DoorState as i32), &mut doors_datum);
    doors_datum.extend(nested(2, &nested(21, &doors)));

    for (case, bytes) in [
        (
            "Payload",
            v_payload(&soc_datum_bytes(), &created_at_bytes(), &top_level),
        ),
        ("Datum", v_payload(&datum_extra, &created_at_bytes(), &[])),
        ("Value", v_payload(&value_extra, &created_at_bytes(), &[])),
        (
            "Timestamp",
            v_payload(&soc_datum_bytes(), &timestamp_extra, &[]),
        ),
        ("Doors", v_payload(&doors_datum, &created_at_bytes(), &[])),
    ] {
        let decoded = decode(TOPIC_V, &bytes, received_at());
        assert_eq!(
            decoded.unprocessed_reason(),
            Some(UnprocessedReason::UnknownFields),
            "{case}"
        );
        let record = vehicle_data(decoded);
        assert!(record.unknown_fields, "{case}");
        assert_eq!(record.unknown_value_variants, 0, "{case}");
        assert_eq!(record.signals.len(), 1, "{case}");
    }
}

#[test]
fn an_unknown_field_in_alerts_errors_and_connectivity_is_reported() {
    let mut alert = VehicleAlert {
        name: "TestAlert".to_owned(),
        audiences: vec![1],
        started_at: None,
        ended_at: None,
    }
    .encode_to_vec();
    unknown_varint(5, &mut alert);
    let mut alerts = nested(1, &alert);
    alerts.extend(nested(2, &created_at_bytes()));

    let mut tags_entry = Vec::new();
    encoding::string::encode(1, &"k".to_owned(), &mut tags_entry);
    encoding::string::encode(2, &"v".to_owned(), &mut tags_entry);
    unknown_varint(3, &mut tags_entry);
    let mut errors = nested(1, &nested(3, &tags_entry));
    errors.extend(nested(2, &created_at_bytes()));

    let mut errors_top = VehicleErrors {
        errors: Vec::new(),
        created_at: Some(proto_time(1_799_999_000, 0)),
        vin: TEST_VIN.to_owned(),
    }
    .encode_to_vec();
    unknown_varint(4, &mut errors_top);

    let mut connectivity = connectivity_message().encode_to_vec();
    unknown_varint(6, &mut connectivity);

    for (topic, bytes) in [
        (TOPIC_ALERTS, alerts),
        (TOPIC_ERRORS, errors),
        (TOPIC_ERRORS, errors_top),
        (TOPIC_CONNECTIVITY, connectivity),
    ] {
        let decoded = decode(topic, &bytes, received_at());
        assert_eq!(
            decoded.unprocessed_reason(),
            Some(UnprocessedReason::UnknownFields),
            "topic {topic:?}"
        );
        assert!(
            !matches!(decoded, Decoded::Unprocessed(_)),
            "topic {topic:?}: the rows are kept"
        );
    }
}

/// An unknown group (wire types 3 and 4) is also an unknown field.
#[test]
fn an_unknown_group_is_reported() {
    let mut group = Vec::new();
    encoding::encode_key(9, WireType::StartGroup, &mut group);
    unknown_varint(1, &mut group);
    encoding::encode_key(9, WireType::EndGroup, &mut group);
    let bytes = v_payload(&soc_datum_bytes(), &created_at_bytes(), &group);
    assert_eq!(
        decode(TOPIC_V, &bytes, received_at()).unprocessed_reason(),
        Some(UnprocessedReason::UnknownFields)
    );
}

/// A datum with a variant that the schema does not have is also an
/// unknown field. The record has one reason: `unknown_value_variant`.
#[test]
fn an_unknown_value_variant_comes_before_unknown_fields() {
    let bytes = v_payload(
        &datum_with_unknown_variant(Field::Soc, 61),
        &created_at_bytes(),
        &[],
    );
    let decoded = decode(TOPIC_V, &bytes, received_at());
    assert_eq!(
        decoded.unprocessed_reason(),
        Some(UnprocessedReason::UnknownValueVariant)
    );
    assert!(vehicle_data(decoded).unknown_fields);
}

/// Encodings that the Go library or another sender can make for known
/// fields are not unknown fields: other field order, a repeated scalar
/// field, an unpacked repeated enum, and map entries that hold the
/// default key and value, as Go writes them.
#[test]
fn other_encodings_of_known_fields_are_not_unknown_fields() {
    // Fields in reverse order, and `vin` twice.
    let mut reversed = Vec::new();
    encoding::bool::encode(4, &true, &mut reversed);
    encoding::string::encode(3, &"OTHER".to_owned(), &mut reversed);
    encoding::string::encode(3, &TEST_VIN.to_owned(), &mut reversed);
    reversed.extend(nested(2, &created_at_bytes()));
    reversed.extend(nested(1, &soc_datum_bytes()));

    // Unpacked `audiences`.
    let mut alert = Vec::new();
    encoding::string::encode(1, &"TestAlert".to_owned(), &mut alert);
    encoding::int32::encode(2, &1, &mut alert);
    encoding::int32::encode(2, &2, &mut alert);
    let mut alerts = nested(1, &alert);
    alerts.extend(nested(2, &created_at_bytes()));

    // Map entries with an explicit empty key and an explicit empty value.
    let mut empty_value = Vec::new();
    encoding::string::encode(1, &"k".to_owned(), &mut empty_value);
    encoding::string::encode(2, &String::new(), &mut empty_value);
    let mut empty_key = Vec::new();
    encoding::string::encode(1, &String::new(), &mut empty_key);
    encoding::string::encode(2, &"v".to_owned(), &mut empty_key);
    let mut error = nested(3, &empty_value);
    error.extend(nested(3, &empty_key));
    let mut errors = nested(1, &error);
    errors.extend(nested(2, &created_at_bytes()));

    for (topic, bytes) in [
        (TOPIC_V, reversed),
        (TOPIC_ALERTS, alerts),
        (TOPIC_ERRORS, errors),
    ] {
        let decoded = decode(topic, &bytes, received_at());
        assert_eq!(
            decoded.unprocessed_reason(),
            None,
            "topic {topic:?}: {decoded:?}"
        );
    }
}

/// The table that `build.rs` writes holds every field of every message
/// type of the schema, with the right nested type.
#[test]
fn the_field_table_matches_the_schema() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/proto");
    let files = [
        "vehicle_data.proto",
        "vehicle_alert.proto",
        "vehicle_error.proto",
        "vehicle_connectivity.proto",
    ]
    .map(|file| format!("{dir}/{file}"));
    let schema = protox::compile(files, [dir]).unwrap();
    let mut names = Vec::new();
    let mut descriptors = Vec::new();
    for file in &schema.file {
        let mut stack: Vec<(String, &DescriptorProto)> = file
            .message_type
            .iter()
            .map(|message| (format!(".{}.{}", file.package(), message.name()), message))
            .collect();
        while let Some((name, message)) = stack.pop() {
            for nested in &message.nested_type {
                stack.push((format!("{name}.{}", nested.name()), nested));
            }
            names.push(name);
            descriptors.push(message);
        }
    }
    assert_eq!(unknown_fields::MESSAGES.len(), names.len());
    for (root, name) in [
        (unknown_fields::PAYLOAD, ".telemetry.vehicle_data.Payload"),
        (
            unknown_fields::VEHICLE_ALERTS,
            ".telemetry.vehicle_alerts.VehicleAlerts",
        ),
        (
            unknown_fields::VEHICLE_ERRORS,
            ".telemetry.vehicle_error.VehicleErrors",
        ),
        (
            unknown_fields::VEHICLE_CONNECTIVITY,
            ".telemetry.vehicle_connectivity.VehicleConnectivity",
        ),
    ] {
        check_table(root, name, &names, &descriptors);
    }
}

/// Compares the table entry `index` with the descriptor `name`, and its
/// nested message types with theirs.
fn check_table(index: usize, name: &str, names: &[String], descriptors: &[&DescriptorProto]) {
    let position = names.iter().position(|known| known == name).unwrap();
    let descriptor = descriptors[position];
    let table = &unknown_fields::MESSAGES[index];
    let mut expected: Vec<i32> = descriptor
        .field
        .iter()
        .map(FieldDescriptorProto::number)
        .collect();
    let mut actual: Vec<i32> = table
        .fields
        .iter()
        .map(|field| i32::try_from(field.number).unwrap())
        .collect();
    expected.sort_unstable();
    actual.sort_unstable();
    assert_eq!(actual, expected, "{name}");
    for field in &descriptor.field {
        let entry = table
            .fields
            .iter()
            .find(|entry| i64::from(entry.number) == i64::from(field.number()))
            .unwrap();
        match (field.r#type(), entry.message) {
            (Type::Message, Some(child)) => {
                check_table(child, field.type_name(), names, descriptors);
            }
            (Type::Message, None) => panic!("{name}.{} has no nested type", field.name()),
            (_, Some(_)) => panic!("{name}.{} is not a message", field.name()),
            (_, None) => {}
        }
    }
}

// --- Privacy -----------------------------------------------------------

/// A `V` payload with one datum of each kind of value, with values that
/// a test can find in a text.
fn vehicle_data_with_every_kind_of_value() -> Payload {
    payload(vec![
        datum(
            Field::Location,
            value::Value::LocationValue(LocationValue {
                latitude: 12.3456,
                longitude: -65.4321,
            }),
        ),
        datum(
            Field::Odometer,
            value::Value::StringValue("secret-text-value".to_owned()),
        ),
        datum(Field::Soc, value::Value::IntValue(4_242_421)),
        datum(Field::Soc, value::Value::LongValue(9_876_543_210)),
        datum(Field::Soc, value::Value::DoubleValue(31_415.92)),
        datum(Field::Soc, value::Value::FloatValue(31_415.92)),
        datum(Field::Gear, value::Value::ShiftStateValue(4_242_421)),
        datum(Field::Soc, value::Value::BooleanValue(true)),
        datum(Field::Soc, value::Value::Invalid(true)),
        datum(
            Field::DoorState,
            value::Value::DoorValue(Doors {
                driver_front: true,
                ..Doors::default()
            }),
        ),
        datum(
            Field::Soc,
            value::Value::TimeValue(Time {
                hour: 21,
                minute: 37,
                second: 53,
            }),
        ),
        datum(
            Field::Soc,
            value::Value::TireLocationValue(TireLocation::default()),
        ),
    ])
}

/// The `Debug` output of a decoded record of each type holds no vehicle
/// value: no VIN, coordinate, signal value, JSON key, or vehicle text.
#[test]
fn debug_output_holds_no_vehicle_values() {
    const SECRETS: [&str; 16] = [
        TEST_VIN,
        "12.3456",
        "65.4321",
        "secret-text-value",
        "4242421",
        "9876543210",
        "31415.92",
        "DriverFront",
        "front_left",
        "minute",
        "secret-alert-name",
        "secret-error-name",
        "secret-tag-key",
        "secret-tag-value",
        "secret-error-body",
        "secret-network",
    ];
    let vehicle = vehicle_data_with_every_kind_of_value();
    let alerts = VehicleAlerts {
        alerts: vec![VehicleAlert {
            name: "secret-alert-name".to_owned(),
            audiences: vec![1],
            started_at: None,
            ended_at: None,
        }],
        created_at: Some(proto_time(1_799_999_000, 0)),
        vin: TEST_VIN.to_owned(),
    };
    let errors = VehicleErrors {
        errors: vec![ProtoVehicleError {
            created_at: None,
            name: "secret-error-name".to_owned(),
            tags: BTreeMap::from([("secret-tag-key".to_owned(), "secret-tag-value".to_owned())]),
            body: "secret-error-body".to_owned(),
        }],
        created_at: Some(proto_time(1_799_999_000, 0)),
        vin: TEST_VIN.to_owned(),
    };
    let connectivity = VehicleConnectivity {
        network_interface: "secret-network".to_owned(),
        ..connectivity_message()
    };
    let decoded = [
        decode(TOPIC_V, &vehicle.encode_to_vec(), received_at()),
        decode(TOPIC_ALERTS, &alerts.encode_to_vec(), received_at()),
        decode(TOPIC_ERRORS, &errors.encode_to_vec(), received_at()),
        decode(
            TOPIC_CONNECTIVITY,
            &connectivity.encode_to_vec(),
            received_at(),
        ),
        decode(b"unknown", &vehicle.encode_to_vec(), received_at()),
    ];
    for record in &decoded {
        assert_eq!(
            record.unprocessed_reason().is_none(),
            !matches!(record, Decoded::Unprocessed(_))
        );
        let text = format!("{record:?} {record:#?}");
        for secret in SECRETS {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }
    let Decoded::VehicleData(record) = &decoded[0] else {
        panic!("expected vehicle data");
    };
    assert_eq!(record.signals.len(), 12);
    // The structure stays visible.
    let text = format!("{record:?}");
    for visible in [
        "Location([redacted])",
        "Text([redacted])",
        "value_type: 9",
        "field: 21",
    ] {
        assert!(text.contains(visible), "{visible} not in {text}");
    }
}

#[test]
fn debug_output_does_not_show_the_vin() {
    let message = payload(vec![datum(Field::Soc, value::Value::IntValue(1))]);
    let text = format!("{:?}", decode_v(&message));
    assert!(!text.contains(TEST_VIN), "{text}");
    assert!(text.contains("Vin([redacted])"), "{text}");

    let text = format!(
        "{:?}",
        decode(
            TOPIC_CONNECTIVITY,
            &connectivity_message().encode_to_vec(),
            received_at()
        )
    );
    assert!(!text.contains(TEST_VIN), "{text}");
}

/// An unprocessed outcome holds only static values: no payload bytes and
/// no text from the protobuf library, which could quote the payload.
#[test]
fn an_unprocessed_outcome_holds_no_payload_data() {
    let mut bytes = payload(vec![datum(
        Field::Location,
        value::Value::StringValue("secret-location-text".to_owned()),
    )])
    .encode_to_vec();
    bytes.push(0xff);
    let decoded = decode(TOPIC_V, &bytes, received_at());
    assert_eq!(reason(&decoded), Some(UnprocessedReason::DecodeFailed));
    let text = format!("{decoded:?}");
    assert!(!text.contains("secret"), "{text}");
    assert!(!text.contains(TEST_VIN), "{text}");
}

// --- Property tests ----------------------------------------------------

mod properties {
    use proptest::prelude::*;

    use super::*;

    fn topic() -> impl Strategy<Value = Vec<u8>> {
        prop_oneof![
            Just(TOPIC_V.to_vec()),
            Just(TOPIC_ALERTS.to_vec()),
            Just(TOPIC_ERRORS.to_vec()),
            Just(TOPIC_CONNECTIVITY.to_vec()),
            proptest::collection::vec(any::<u8>(), 0..40),
        ]
    }

    fn signal_value() -> impl Strategy<Value = Option<value::Value>> {
        prop_oneof![
            Just(None),
            any::<String>().prop_map(|text| Some(value::Value::StringValue(text))),
            any::<i32>().prop_map(|number| Some(value::Value::IntValue(number))),
            any::<i64>().prop_map(|number| Some(value::Value::LongValue(number))),
            any::<f64>().prop_map(|number| Some(value::Value::DoubleValue(number))),
            any::<bool>().prop_map(|flag| Some(value::Value::Invalid(flag))),
            any::<i32>().prop_map(|number| Some(value::Value::ChargingValue(number))),
        ]
    }

    proptest! {
        /// Arbitrary bytes under any topic: the decode returns an outcome
        /// and does not panic.
        #[test]
        fn arbitrary_bytes_do_not_panic(
            topic in topic(),
            bytes in proptest::collection::vec(any::<u8>(), 0..512),
        ) {
            let decoded = decode(&topic, &bytes, received_at());
            if RecordType::from_topic(&topic).is_none() {
                prop_assert_eq!(
                    decoded.unprocessed_reason(),
                    Some(UnprocessedReason::UnknownTopic)
                );
            }
        }

        /// A valid `V` record: every datum is either a signal or counted,
        /// in order, and the time is truncated and never moves forward.
        #[test]
        fn every_datum_is_a_signal_or_counted(
            datums in proptest::collection::vec((any::<i32>(), signal_value()), 0..20),
            seconds in MIN_TIMESTAMP_SECONDS..253_402_128_000_i64,
            nanos in 0..1_000_000_000_i32,
        ) {
            let message = Payload {
                data: datums
                    .iter()
                    .map(|(key, value)| Datum {
                        key: *key,
                        value: Some(vehicle_data::Value { value: value.clone() }),
                    })
                    .collect(),
                created_at: Some(proto_time(seconds, nanos)),
                vin: TEST_VIN.to_owned(),
                is_resend: false,
            };
            let record = vehicle_data(decode_v(&message));
            let known: Vec<i32> = datums
                .iter()
                .filter(|(_, value)| value.is_some())
                .map(|(key, _)| *key)
                .collect();
            let fields: Vec<i32> = record.signals.iter().map(|signal| signal.field).collect();
            prop_assert_eq!(fields, known);
            // A payload that `prost` encodes from the schema has no
            // unknown field.
            prop_assert!(!record.unknown_fields);
            prop_assert_eq!(
                record.unknown_value_variants,
                datums.len() - record.signals.len()
            );
            let sent = i128::from(seconds) * 1_000_000_000 + i128::from(nanos);
            let stored = record.created_at.as_nanosecond();
            prop_assert_eq!(stored % 1_000, 0);
            prop_assert!(stored <= sent && sent - stored < 1_000);
        }
    }
}
