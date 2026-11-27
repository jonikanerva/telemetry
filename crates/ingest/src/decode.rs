//! Decode of the `fleet-telemetry` ZMQ messages into typed raw records.
//!
//! The decode is pure: no network, no database, no clock, and no
//! environment. The caller passes the receive time. The output types are in
//! [`crate::record`].
//!
//! # What a ZMQ message holds
//!
//! The sources are the upstream files at tag `v0.10.1`, commit
//! `6689adb831691b1de211b4aa8220babf7309f31c`
//! (<https://github.com/teslamotors/fleet-telemetry/tree/6689adb831691b1de211b4aa8220babf7309f31c>).
//!
//! - The ZMQ producer sends one message with two frames: the topic, then
//!   the record payload (`datastore/zmq/zmq.go`, line 77). The message has
//!   no transaction ID and no receive time.
//! - The topic is `<namespace>_<record type>` (`telemetry/producer.go`,
//!   lines 29-32). The record type is the topic of the vehicle message
//!   (`telemetry/serializer.go`, line 56), or `connectivity` for the
//!   events that the server makes itself (`server/streaming/server.go`,
//!   lines 38-40 and 149-191). The server configuration sets the namespace
//!   `tesla_telemetry` ([`TOPIC_PREFIX`](crate::record::TOPIC_PREFIX)).
//! - The payload is `Record.PayloadBytes` (`telemetry/record.go`, lines
//!   92-95), first the payload of the vehicle message
//!   (`telemetry/serializer.go`, line 59). For the record types `V`, `alerts`, `errors`, and
//!   `connectivity`, the server decodes the vehicle payload, sets the VIN
//!   of the connection, changes some values, and encodes the message again
//!   with protobuf (`telemetry/record.go`, lines 152-201). With
//!   `transmit_decoded_records` set to `false`, the payload stays protobuf;
//!   it becomes JSON only when the setting is `true` (`telemetry/record.go`,
//!   lines 203-213). ADR 0001 sets it to `false`.
//! - The changes before the encode (ADR 0001, "Wire format: protobuf"):
//!   a `Location` text that parses becomes a `location_value`
//!   (lines 228-240); a text in scientific notation becomes a text with
//!   five decimals (lines 265-277); the AEB enum of client 1.3.0 shifts by
//!   one (lines 242-263); an alert time with seconds in milliseconds
//!   becomes a correct time (lines 302-317). The decode stores the values
//!   after these changes and does not repeat or undo them.
//! - The connection ID of a `connectivity` record is the `X-TXID` header
//!   when it is a UUID, or a random UUID, as text
//!   (`server/streaming/socket.go`, lines 91 and 105-128). Its `created_at`
//!   is the server clock (`server/streaming/server.go`, line 159).
//!
//! # Outcomes
//!
//! [`decode`] has no error result. Each message gives one [`Decoded`]
//! value, and the property tests check that arbitrary bytes do not cause
//! a panic. Nothing is dropped without a trace: every message that cannot
//! become rows has an [`UnprocessedReason`] (ADR 0002, "Unknown data"),
//! and the storage then stores its original bytes.
//!
//! | Case | Outcome |
//! | ---- | ------- |
//! | The topic is not [`TOPIC_PREFIX`](crate::record::TOPIC_PREFIX) followed by a known record type | `unknown_topic` |
//! | The payload is not a valid protobuf message of the record type | `decode_failed` |
//! | The record has no `created_at` | `missing_created_at` |
//! | A timestamp is outside the range of `google.protobuf.Timestamp`, or its nanoseconds are outside 0 to 999,999,999 | `decode_failed` |
//! | A connection ID is not a UUID | `decode_failed` |
//! | Some datums of a `V` record have no known value variant | Rows for the other datums, and `unknown_value_variant` |
//! | The payload holds a field that the vendored schema does not know | All rows, and `unknown_fields` |
//!
//! The checks run in this order: topic, protobuf, `created_at`, the other
//! values. The first failed check gives the reason. A failure of the
//! message as a whole stops its decode. Partial success occurs only where
//! ADR 0002 allows it: datums without a known value variant, and fields
//! that the vendored schema does not know (`unknown_fields`). In both
//! cases the rows that the schema describes are kept, and the storage
//! also keeps the original bytes, so a later step can read the rest.
//!
//! The `created_at` rule applies to all four record types, as ADR 0002
//! says ("Unknown data"). The time of an alert or error item can be
//! missing; it is then `None`, and the storage writes NULL.

pub mod proto;
#[cfg(test)]
mod tests;
mod unknown_fields;

use jiff::Timestamp;
use prost::Message;
use uuid::Uuid;

use self::proto::vehicle_alerts::{VehicleAlert, VehicleAlerts};
use self::proto::vehicle_connectivity::VehicleConnectivity;
use self::proto::vehicle_data::{Datum, Doors, LocationValue, Payload, Time, TireLocation, value};
use self::proto::vehicle_error::{VehicleError as ProtoVehicleError, VehicleErrors};
use self::unknown_fields::has_unknown_fields;
use crate::record::{
    Alert, Alerts, Connectivity, Decoded, EnumValue, Errors, JsonObject, Location, RecordType,
    Signal, SignalValue, Unprocessed, UnprocessedReason, VehicleData, VehicleError, Vin,
};

/// Decodes one ZMQ message.
///
/// `topic` and `payload` are the two frames of the message. `received_at`
/// is the clock of the ingest process when it read the message (ADR 0002,
/// "Time columns"); the records hold it unchanged.
#[must_use]
pub fn decode(topic: &[u8], payload: &[u8], received_at: Timestamp) -> Decoded {
    let Some(record_type) = RecordType::from_topic(topic) else {
        return Decoded::Unprocessed(Unprocessed {
            reason: UnprocessedReason::UnknownTopic,
            received_at,
        });
    };
    let decoded = match record_type {
        RecordType::VehicleData => vehicle_data(payload, received_at).map(Decoded::VehicleData),
        RecordType::Alerts => alerts(payload, received_at).map(Decoded::Alerts),
        RecordType::Errors => errors(payload, received_at).map(Decoded::Errors),
        RecordType::Connectivity => connectivity(payload, received_at).map(Decoded::Connectivity),
    };
    decoded.unwrap_or_else(|reason| {
        Decoded::Unprocessed(Unprocessed {
            reason,
            received_at,
        })
    })
}

fn protobuf<M: Message + Default>(payload: &[u8]) -> Result<M, UnprocessedReason> {
    // The text of a `prost::DecodeError` is not kept: the outcome holds
    // static values only.
    M::decode(payload).map_err(|_| UnprocessedReason::DecodeFailed)
}

fn vehicle_data(payload: &[u8], received_at: Timestamp) -> Result<VehicleData, UnprocessedReason> {
    let Payload {
        data,
        created_at,
        vin,
        is_resend,
    } = protobuf(payload)?;
    let unknown_fields = has_unknown_fields(payload, unknown_fields::PAYLOAD)?;
    let created_at = required_instant(created_at.as_ref())?;
    let mut signals = Vec::with_capacity(data.len());
    let mut unknown_value_variants = 0;
    for Datum { key, value } in data {
        match value.and_then(|value| value.value) {
            Some(value) => signals.push(Signal {
                field: key,
                value: signal_value(value),
            }),
            None => unknown_value_variants += 1,
        }
    }
    Ok(VehicleData {
        vin: Vin::new(vin),
        created_at,
        received_at,
        is_resend,
        signals,
        unknown_value_variants,
        unknown_fields,
    })
}

fn alerts(payload: &[u8], received_at: Timestamp) -> Result<Alerts, UnprocessedReason> {
    let VehicleAlerts {
        alerts,
        created_at,
        vin,
    } = protobuf(payload)?;
    let unknown_fields = has_unknown_fields(payload, unknown_fields::VEHICLE_ALERTS)?;
    let created_at = required_instant(created_at.as_ref())?;
    let alerts = alerts
        .into_iter()
        .map(|alert| {
            let VehicleAlert {
                name,
                audiences,
                started_at,
                ended_at,
            } = alert;
            Ok(Alert {
                name,
                audiences,
                started_at: optional_instant(started_at.as_ref())?,
                ended_at: optional_instant(ended_at.as_ref())?,
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(Alerts {
        vin: Vin::new(vin),
        created_at,
        received_at,
        alerts,
        unknown_fields,
    })
}

fn errors(payload: &[u8], received_at: Timestamp) -> Result<Errors, UnprocessedReason> {
    let VehicleErrors {
        errors,
        created_at,
        vin,
    } = protobuf(payload)?;
    let unknown_fields = has_unknown_fields(payload, unknown_fields::VEHICLE_ERRORS)?;
    let created_at = required_instant(created_at.as_ref())?;
    let errors = errors
        .into_iter()
        .map(|error| {
            let ProtoVehicleError {
                created_at,
                name,
                tags,
                body,
            } = error;
            Ok(VehicleError {
                created_at: optional_instant(created_at.as_ref())?,
                name,
                tags,
                body,
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(Errors {
        vin: Vin::new(vin),
        created_at,
        received_at,
        errors,
        unknown_fields,
    })
}

fn connectivity(payload: &[u8], received_at: Timestamp) -> Result<Connectivity, UnprocessedReason> {
    let VehicleConnectivity {
        vin,
        connection_id,
        status,
        created_at,
        network_interface,
    } = protobuf(payload)?;
    let unknown_fields = has_unknown_fields(payload, unknown_fields::VEHICLE_CONNECTIVITY)?;
    let created_at = required_instant(created_at.as_ref())?;
    let connection_id =
        Uuid::try_parse(&connection_id).map_err(|_| UnprocessedReason::DecodeFailed)?;
    Ok(Connectivity {
        vin: Vin::new(vin),
        connection_id,
        status,
        network_interface,
        created_at,
        received_at,
        unknown_fields,
    })
}

/// Seconds of `0001-01-01T00:00:00Z`, the earliest valid
/// `google.protobuf.Timestamp`.
const MIN_TIMESTAMP_SECONDS: i64 = -62_135_596_800;
/// Seconds of `9999-12-31T23:59:59Z`, the latest valid
/// `google.protobuf.Timestamp`.
const MAX_TIMESTAMP_SECONDS: i64 = 253_402_300_799;

/// Converts a `google.protobuf.Timestamp` and truncates it to whole
/// microseconds.
///
/// The well-known type allows the seconds of 0001-01-01 to 9999-12-31
/// and nanoseconds from 0 to 999,999,999. A value outside these ranges is
/// not valid, so the decode keeps it as unprocessed instead of a changed
/// time. `jiff` ends about one day earlier, at
/// `9999-12-30T22:00:00.999999999Z`; a later value is also unprocessed.
///
/// The nanoseconds always count forward from the seconds, also before
/// 1970, so dropping their sub-microsecond part moves the instant back in
/// time and never forward.
fn instant(timestamp: &prost_types::Timestamp) -> Result<Timestamp, UnprocessedReason> {
    let prost_types::Timestamp { seconds, nanos } = *timestamp;
    if !(MIN_TIMESTAMP_SECONDS..=MAX_TIMESTAMP_SECONDS).contains(&seconds)
        || !(0..=999_999_999).contains(&nanos)
    {
        return Err(UnprocessedReason::DecodeFailed);
    }
    Timestamp::new(seconds, nanos - nanos % 1_000).map_err(|_| UnprocessedReason::DecodeFailed)
}

fn required_instant(
    timestamp: Option<&prost_types::Timestamp>,
) -> Result<Timestamp, UnprocessedReason> {
    instant(timestamp.ok_or(UnprocessedReason::MissingCreatedAt)?)
}

fn optional_instant(
    timestamp: Option<&prost_types::Timestamp>,
) -> Result<Option<Timestamp>, UnprocessedReason> {
    timestamp.map(instant).transpose()
}

/// Maps a variant of the `Value.value` oneof.
///
/// The numbers are the protobuf field numbers of the variants in
/// `proto/vehicle_data.proto`. `prost` does not expose them, so they are
/// written here. A test compares each number with the schema. The match
/// has no wildcard arm: a new variant in the schema stops the build until
/// it has a number here.
fn signal_value(value: value::Value) -> SignalValue {
    use value::Value as V;

    let enum_value = |value_type, number| SignalValue::Enum(EnumValue::new(value_type, number));
    match value {
        V::StringValue(text) => SignalValue::Text(text),
        V::IntValue(number) => SignalValue::Int32(number),
        V::LongValue(number) => SignalValue::Int64(number),
        V::FloatValue(number) => SignalValue::Float32(number),
        V::DoubleValue(number) => SignalValue::Float64(number),
        V::BooleanValue(flag) => SignalValue::Bool(flag),
        V::LocationValue(location) => SignalValue::Location(location_value(location)),
        V::Invalid(marker) => SignalValue::Invalid(marker),
        V::DoorValue(doors) => SignalValue::Doors(doors_object(doors)),
        V::TimeValue(time) => SignalValue::Time(time_object(time)),
        V::TireLocationValue(tires) => SignalValue::TireLocation(tire_location_object(tires)),
        V::ChargingValue(number) => enum_value(8, number),
        V::ShiftStateValue(number) => enum_value(9, number),
        V::LaneAssistLevelValue(number) => enum_value(11, number),
        V::ScheduledChargingModeValue(number) => enum_value(12, number),
        V::SentryModeStateValue(number) => enum_value(13, number),
        V::SpeedAssistLevelValue(number) => enum_value(14, number),
        V::BmsStateValue(number) => enum_value(15, number),
        V::BuckleStatusValue(number) => enum_value(16, number),
        V::CarTypeValue(number) => enum_value(17, number),
        V::ChargePortValue(number) => enum_value(18, number),
        V::ChargePortLatchValue(number) => enum_value(19, number),
        V::DriveInverterStateValue(number) => enum_value(22, number),
        V::HvilStatusValue(number) => enum_value(23, number),
        V::WindowStateValue(number) => enum_value(24, number),
        V::SeatFoldPositionValue(number) => enum_value(25, number),
        V::TractorAirStatusValue(number) => enum_value(26, number),
        V::FollowDistanceValue(number) => enum_value(27, number),
        V::ForwardCollisionSensitivityValue(number) => enum_value(28, number),
        V::GuestModeMobileAccessValue(number) => enum_value(29, number),
        V::TrailerAirStatusValue(number) => enum_value(30, number),
        V::DetailedChargeStateValue(number) => enum_value(32, number),
        V::HvacAutoModeValue(number) => enum_value(33, number),
        V::CabinOverheatProtectionModeValue(number) => enum_value(34, number),
        V::CabinOverheatProtectionTemperatureLimitValue(number) => enum_value(35, number),
        V::DefrostModeValue(number) => enum_value(36, number),
        V::ClimateKeeperModeValue(number) => enum_value(37, number),
        V::HvacPowerValue(number) => enum_value(38, number),
        V::FastChargerValue(number) => enum_value(40, number),
        V::CableTypeValue(number) => enum_value(41, number),
        V::TonneauTentModeValue(number) => enum_value(42, number),
        V::TonneauPositionValue(number) => enum_value(43, number),
        V::PowershareTypeValue(number) => enum_value(44, number),
        V::PowershareStateValue(number) => enum_value(45, number),
        V::PowershareStopReasonValue(number) => enum_value(46, number),
        V::DisplayStateValue(number) => enum_value(47, number),
        V::DistanceUnitValue(number) => enum_value(48, number),
        V::TemperatureUnitValue(number) => enum_value(49, number),
        V::PressureUnitValue(number) => enum_value(50, number),
        V::ChargeUnitPreferenceValue(number) => enum_value(51, number),
        V::TurnSignalStateValue(number) => enum_value(52, number),
        V::MediaStatusValue(number) => enum_value(53, number),
        V::SunroofInstalledStateValue(number) => enum_value(54, number),
        V::CabinPortKeepOnValue(number) => enum_value(55, number),
        V::PowerTransferStatusValue(number) => enum_value(57, number),
        V::AutomaticEmergencyBrakingStateValue(number) => enum_value(60, number),
    }
}

fn location_value(location: LocationValue) -> Location {
    let LocationValue {
        latitude,
        longitude,
    } = location;
    Location {
        latitude,
        longitude,
    }
}

// The keys below are the field names of `proto/vehicle_data.proto`. The
// destructures have no `..`, so a new field in the schema stops the build
// until it has a key here.

fn doors_object(doors: Doors) -> JsonObject {
    let Doors {
        driver_front,
        driver_rear,
        passenger_front,
        passenger_rear,
        trunk_front,
        trunk_rear,
    } = doors;
    json_object([
        ("DriverFront", driver_front.into()),
        ("DriverRear", driver_rear.into()),
        ("PassengerFront", passenger_front.into()),
        ("PassengerRear", passenger_rear.into()),
        ("TrunkFront", trunk_front.into()),
        ("TrunkRear", trunk_rear.into()),
    ])
}

fn time_object(time: Time) -> JsonObject {
    let Time {
        hour,
        minute,
        second,
    } = time;
    json_object([
        ("hour", hour.into()),
        ("minute", minute.into()),
        ("second", second.into()),
    ])
}

fn tire_location_object(tires: TireLocation) -> JsonObject {
    let TireLocation {
        front_left,
        front_right,
        rear_left,
        rear_right,
        semi_middle_axle_left_2,
        semi_middle_axle_right_2,
        semi_rear_axle_left,
        semi_rear_axle_right,
        semi_rear_axle_left_2,
        semi_rear_axle_right_2,
    } = tires;
    json_object([
        ("front_left", front_left.into()),
        ("front_right", front_right.into()),
        ("rear_left", rear_left.into()),
        ("rear_right", rear_right.into()),
        ("semi_middle_axle_left_2", semi_middle_axle_left_2.into()),
        ("semi_middle_axle_right_2", semi_middle_axle_right_2.into()),
        ("semi_rear_axle_left", semi_rear_axle_left.into()),
        ("semi_rear_axle_right", semi_rear_axle_right.into()),
        ("semi_rear_axle_left_2", semi_rear_axle_left_2.into()),
        ("semi_rear_axle_right_2", semi_rear_axle_right_2.into()),
    ])
}

fn json_object<const N: usize>(entries: [(&str, serde_json::Value); N]) -> JsonObject {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}
