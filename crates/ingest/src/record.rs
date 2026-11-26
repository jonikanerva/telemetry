//! Typed raw records: the interface between the decode and the storage.
//!
//! [`crate::decode::decode`] makes one [`Decoded`] value from each ZMQ
//! message. The storage writes the rows of ADR 0002 from these types. It
//! does not read protobuf. Each field below names its source in the
//! vendored schema (`proto/`) and its target column in ADR 0002.
//!
//! Rules that every value of these types keeps:
//!
//! - **Source meaning.** A value is the value that `fleet-telemetry`
//!   dispatched, after the upstream changes that ADR 0001 names ("Wire
//!   format: protobuf"). The decode does not convert units, parse text, or
//!   replace a missing value with zero. Protobuf 3 itself cannot tell a
//!   zero, `false`, or empty text from a missing scalar; such a value is
//!   kept as the zero value that the wire gives.
//! - **Numbers stay numbers.** Field and enum numbers are the wire numbers
//!   (`i32`), also when the vendored schema does not know them.
//! - **Time.** Every instant from the payload is a [`Timestamp`] truncated
//!   to whole microseconds, because PostgreSQL keeps microseconds (ADR 0002,
//!   "Time columns"). Truncation drops the sub-microsecond part, so the
//!   stored instant is never later than the sent instant. A missing instant
//!   is never invented: a record without `created_at` is an
//!   [`Unprocessed`] outcome, and a missing time of an alert or error
//!   item is `None`. The receive time is the value that the caller
//!   passed to the decode, unchanged.
//! - **Privacy.** A record holds the VIN, the location, signal values, and
//!   vehicle text. The `Debug` output of every type here writes
//!   `[redacted]` for these values: the VIN, every [`SignalValue`] payload
//!   (also the coordinates and the JSON objects), the enum number, the
//!   name of an alert, the name, tags, and body of an error, and the
//!   network interface. It shows only the structure: variant names,
//!   `value_type`, field numbers, counts, flags, times, the enum numbers
//!   of the record itself (status, audiences), and the connection ID. The
//!   logging allowlist writes the `Debug` text of an allowed field without
//!   change, so this rule keeps an accidental `?record` out of the log.
//!   Still, do not log a record or put one into an error.

use std::collections::BTreeMap;
use std::fmt;

use jiff::Timestamp;
use uuid::Uuid;

/// The namespace prefix of every topic. `fleet-telemetry` names a topic
/// `<namespace>_<record type>`, and the server configuration sets the
/// namespace `tesla_telemetry` (ADR 0001, fact 4 and "Transport").
pub const TOPIC_PREFIX: &str = "tesla_telemetry_";

/// The record types that `fleet-telemetry` dispatches to the ingest process
/// (ADR 0001, "`fleet-telemetry` configuration").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecordType {
    /// Topic `V`, protobuf message `Payload`: vehicle signals.
    VehicleData,
    /// Topic `alerts`, protobuf message `VehicleAlerts`.
    Alerts,
    /// Topic `errors`, protobuf message `VehicleErrors`.
    Errors,
    /// Topic `connectivity`, protobuf message `VehicleConnectivity`. The
    /// server makes these records itself, with its own clock.
    Connectivity,
}

impl RecordType {
    /// Every record type.
    pub const ALL: [Self; 4] = [
        Self::VehicleData,
        Self::Alerts,
        Self::Errors,
        Self::Connectivity,
    ];

    /// The upstream name of the record type, for example `V`. The name is
    /// static text, so a log event can use it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::VehicleData => "V",
            Self::Alerts => "alerts",
            Self::Errors => "errors",
            Self::Connectivity => "connectivity",
        }
    }

    /// Finds the record type of a complete ZMQ topic, for example
    /// `tesla_telemetry_V`. The topic must be exactly [`TOPIC_PREFIX`]
    /// followed by a record type name.
    #[must_use]
    pub fn from_topic(topic: &[u8]) -> Option<Self> {
        let name = topic.strip_prefix(TOPIC_PREFIX.as_bytes())?;
        Self::ALL
            .into_iter()
            .find(|record_type| record_type.name().as_bytes() == name)
    }
}

/// The result of the decode of one ZMQ message.
///
/// The original topic and payload bytes are not part of this value. The
/// queue item keeps them (ADR 0001, "Delivery path"), and the storage
/// writes them to `raw.unprocessed_record` when
/// [`Decoded::unprocessed_reason`] returns a reason.
#[derive(Debug, Clone, PartialEq)]
pub enum Decoded {
    /// A `V` record. Its signals go to `raw.vehicle_signal`.
    VehicleData(VehicleData),
    /// An `alerts` record. Its items go to `raw.vehicle_alert`.
    Alerts(Alerts),
    /// An `errors` record. Its items go to `raw.vehicle_error`.
    Errors(Errors),
    /// A `connectivity` record. It goes to `raw.connectivity_event`.
    Connectivity(Connectivity),
    /// No rows. The complete message goes to `raw.unprocessed_record`.
    Unprocessed(Unprocessed),
}

impl Decoded {
    /// The reason to store the complete message in
    /// `raw.unprocessed_record`, or `None`.
    ///
    /// A record can have both rows and a reason (ADR 0002, "Unknown
    /// data"): the rows are stored, and the complete message is also
    /// stored as unprocessed. This occurs when some datums of a `V` record
    /// have no known value variant
    /// ([`UnprocessedReason::UnknownValueVariant`]), or when a record of
    /// any type holds a field that the vendored schema does not know
    /// ([`UnprocessedReason::UnknownFields`]). A record has one reason. An
    /// unknown value variant is also an unknown field, so
    /// `unknown_value_variant` comes first; the stored bytes are the same.
    #[must_use]
    pub fn unprocessed_reason(&self) -> Option<UnprocessedReason> {
        let unknown_fields = match self {
            Self::VehicleData(record) if record.unknown_value_variants > 0 => {
                return Some(UnprocessedReason::UnknownValueVariant);
            }
            Self::Unprocessed(unprocessed) => return Some(unprocessed.reason),
            Self::VehicleData(record) => record.unknown_fields,
            Self::Alerts(record) => record.unknown_fields,
            Self::Errors(record) => record.unknown_fields,
            Self::Connectivity(record) => record.unknown_fields,
        };
        unknown_fields.then_some(UnprocessedReason::UnknownFields)
    }
}

/// A message that the decode cannot turn into rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unprocessed {
    /// Column `reason`.
    pub reason: UnprocessedReason,
    /// Column `received_at`: the receive time that the caller passed.
    pub received_at: Timestamp,
}

/// Why a message goes to `raw.unprocessed_record` (ADR 0002, column
/// `reason`). ADR 0002 also names `conflicting_duplicate` and
/// `database_rejected`. Only the storage can find those cases, so the
/// storage adds them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnprocessedReason {
    /// The payload is not a valid protobuf message of the record type, or
    /// it holds a value that the record type cannot hold: a timestamp
    /// outside the range of `google.protobuf.Timestamp`, or a connection
    /// ID that is not a UUID.
    DecodeFailed,
    /// The topic is not [`TOPIC_PREFIX`] followed by a known record type.
    ///
    /// The topic is the first frame of the ZMQ message, as bytes, so it can
    /// be any bytes: empty, of any length, not UTF-8, or with a NUL byte.
    /// Upstream sends only `<namespace>_<record type>` with the record
    /// types of the server configuration, but the decode does not rely on
    /// it. The storage must handle such bytes before it writes the `text`
    /// column `topic`.
    UnknownTopic,
    /// The record has no `created_at` (`Payload`, `VehicleAlerts`,
    /// `VehicleErrors`, or `VehicleConnectivity`). ADR 0002 sends such a
    /// record to `raw.unprocessed_record` as a whole and never gives it an
    /// invented time. A time of an alert or error item can be missing; it
    /// is `None`.
    MissingCreatedAt,
    /// At least one datum of a `V` record has no value variant that the
    /// vendored schema knows. See [`Decoded::unprocessed_reason`].
    UnknownValueVariant,
    /// The payload holds a field that the vendored schema does not know,
    /// at any depth, for example a field that a newer schema added to
    /// `Payload` or `Datum`. `prost` skips such a field, so the record
    /// keeps its rows, and the complete message is also stored. See
    /// [`Decoded::unprocessed_reason`].
    UnknownFields,
}

impl UnprocessedReason {
    /// The text of the `reason` column. It is static text, so a log event
    /// can use it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DecodeFailed => "decode_failed",
            Self::UnknownTopic => "unknown_topic",
            Self::MissingCreatedAt => "missing_created_at",
            Self::UnknownValueVariant => "unknown_value_variant",
            Self::UnknownFields => "unknown_fields",
        }
    }
}

/// A vehicle identification number, as the record holds it.
///
/// Source: the `vin` field of each record type. `fleet-telemetry` sets it
/// from the client certificate of the connection. The decode does not
/// check its form. Target: `raw.vehicle.vin`; the other tables hold the
/// vehicle ID.
///
/// `Debug` writes `[redacted]`. The type does not implement `Display`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Vin(String);

impl Vin {
    /// Wraps a VIN.
    #[must_use]
    pub fn new(vin: String) -> Self {
        Self(vin)
    }

    /// Returns the VIN. Call it only where the storage writes the value.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Vin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Vin").field(&Redacted).finish()
    }
}

/// Writes `[redacted]` in place of a value in the `Debug` output of the
/// record types. See the module documentation, "Privacy".
struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// A `V` record: protobuf message `Payload`.
#[derive(Debug, Clone, PartialEq)]
pub struct VehicleData {
    /// `Payload.vin`.
    pub vin: Vin,
    /// `Payload.created_at`, the vehicle clock. Column `created_at` of each
    /// signal row.
    pub created_at: Timestamp,
    /// Column `received_at` of each signal row.
    pub received_at: Timestamp,
    /// `Payload.is_resend`. Column `is_resend` of each signal row.
    pub is_resend: bool,
    /// One signal for each datum with a known value variant, in the order
    /// of `Payload.data`. Two signals can have the same field; the storage
    /// handles such keys (ADR 0002, "Idempotency").
    pub signals: Vec<Signal>,
    /// The number of datums that have no value variant that the vendored
    /// schema knows: the `Value` is missing, it has no variant set, or it
    /// has a newer variant that `prost` skips. These datums have no
    /// signal. When the number is above zero, the complete message also
    /// goes to `raw.unprocessed_record`.
    pub unknown_value_variants: usize,
    /// The payload holds a field that the vendored schema does not know.
    /// See [`UnprocessedReason::UnknownFields`].
    pub unknown_fields: bool,
}

/// One datum of a `V` record: one row of `raw.vehicle_signal`.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    /// `Datum.key`: the number of the `Field` enum, also when the vendored
    /// schema does not know it. Column `field`.
    pub field: i32,
    /// `Datum.value`. Columns `value_type` and one value column.
    pub value: SignalValue,
}

/// The value of a signal: one variant of the `Value.value` oneof.
///
/// [`SignalValue::value_type`] gives the column `value_type`. The value
/// column follows from the variant (ADR 0002, "Value representation"):
///
/// | Variant | Column |
/// | ------- | ------ |
/// | [`Text`](Self::Text) | `text_value` |
/// | [`Int32`](Self::Int32), [`Int64`](Self::Int64), [`Enum`](Self::Enum) | `int_value` |
/// | [`Float32`](Self::Float32), [`Float64`](Self::Float64) | `float_value` |
/// | [`Bool`](Self::Bool), [`Invalid`](Self::Invalid) | `bool_value` |
/// | [`Location`](Self::Location) | `latitude`, `longitude` |
/// | [`Doors`](Self::Doors), [`Time`](Self::Time), [`TireLocation`](Self::TireLocation) | `json_value` |
///
/// `i32` widens to `i64` and `f32` widens to `f64` without loss
/// (`i64::from`, `f64::from`). `PartialEq` follows IEEE 754 for floats: a
/// NaN is not equal to a NaN. A comparison with a stored value must
/// define its own rule for NaN.
#[derive(Clone, PartialEq)]
pub enum SignalValue {
    /// `string_value` (1), as sent. `"12.3"` stays text.
    Text(String),
    /// `int_value` (2), protobuf `int32`.
    Int32(i32),
    /// `long_value` (3), protobuf `int64`.
    Int64(i64),
    /// `float_value` (4), protobuf `float`.
    Float32(f32),
    /// `double_value` (5), protobuf `double`.
    Float64(f64),
    /// `boolean_value` (6).
    Bool(bool),
    /// `location_value` (7).
    Location(Location),
    /// `invalid` (10): the vehicle marks that it has no valid value for the
    /// field. It is not a value of zero or `false`. The `bool` is the
    /// marker as sent.
    Invalid(bool),
    /// One of the enum variants, for example `charging_value` (8).
    Enum(EnumValue),
    /// `door_value` (21): message `Doors` as a JSON object.
    Doors(JsonObject),
    /// `time_value` (31): message `Time` as a JSON object.
    Time(JsonObject),
    /// `tire_location_value` (39): message `TireLocation` as a JSON object.
    TireLocation(JsonObject),
}

/// A JSON object with one key for each field of a protobuf message. The
/// key is the field name as the `.proto` file spells it, for example
/// `DriverFront` or `front_left`. Every field is present, also `false`
/// and 0 (ADR 0002, "Value representation").
pub type JsonObject = serde_json::Map<String, serde_json::Value>;

impl SignalValue {
    /// The protobuf field number of the variant in the `Value.value`
    /// oneof. Column `value_type`.
    #[must_use]
    pub fn value_type(&self) -> i16 {
        match self {
            Self::Text(_) => 1,
            Self::Int32(_) => 2,
            Self::Int64(_) => 3,
            Self::Float32(_) => 4,
            Self::Float64(_) => 5,
            Self::Bool(_) => 6,
            Self::Location(_) => 7,
            Self::Invalid(_) => 10,
            Self::Enum(value) => value.value_type(),
            Self::Doors(_) => 21,
            Self::Time(_) => 31,
            Self::TireLocation(_) => 39,
        }
    }
}

/// `location_value` (7), message `LocationValue`, in degrees as sent.
#[derive(Clone, Copy, PartialEq)]
pub struct Location {
    /// `LocationValue.latitude`. Column `latitude`.
    pub latitude: f64,
    /// `LocationValue.longitude`. Column `longitude`.
    pub longitude: f64,
}

/// An enum variant of the `Value.value` oneof.
///
/// Only the decode makes this value, so the value type is always the
/// field number of an enum variant in the vendored schema.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnumValue {
    value_type: i16,
    number: i32,
}

impl EnumValue {
    /// Only the decode calls this, with the field numbers of the schema.
    pub(crate) const fn new(value_type: i16, number: i32) -> Self {
        Self { value_type, number }
    }

    /// The protobuf field number of the variant, for example 8 for
    /// `charging_value`. Column `value_type`.
    #[must_use]
    pub const fn value_type(self) -> i16 {
        self.value_type
    }

    /// The enum number as sent, also when the vendored schema does not
    /// know it. Column `int_value`.
    #[must_use]
    pub const fn number(self) -> i32 {
        self.number
    }
}

/// An `alerts` record: protobuf message `VehicleAlerts`.
#[derive(Debug, Clone, PartialEq)]
pub struct Alerts {
    /// `VehicleAlerts.vin`.
    pub vin: Vin,
    /// `VehicleAlerts.created_at`. Column `created_at` of each alert row.
    pub created_at: Timestamp,
    /// Column `received_at` of each alert row.
    pub received_at: Timestamp,
    /// `VehicleAlerts.alerts`, in the order of the record.
    pub alerts: Vec<Alert>,
    /// The payload holds a field that the vendored schema does not know.
    /// See [`UnprocessedReason::UnknownFields`].
    pub unknown_fields: bool,
}

/// One alert: protobuf message `VehicleAlert`, one row of
/// `raw.vehicle_alert`.
#[derive(Clone, PartialEq, Eq)]
pub struct Alert {
    /// `VehicleAlert.name`. Column `name`.
    pub name: String,
    /// `VehicleAlert.audiences`: the numbers of the `Audience` enum, in the
    /// order of the record. Column `audiences`.
    pub audiences: Vec<i32>,
    /// `VehicleAlert.started_at`. Column `started_at`.
    pub started_at: Option<Timestamp>,
    /// `VehicleAlert.ended_at`. `None` while the alert is active. Column
    /// `ended_at`.
    pub ended_at: Option<Timestamp>,
}

/// An `errors` record: protobuf message `VehicleErrors`.
#[derive(Debug, Clone, PartialEq)]
pub struct Errors {
    /// `VehicleErrors.vin`.
    pub vin: Vin,
    /// `VehicleErrors.created_at`. Column `record_created_at` of each error
    /// row.
    pub created_at: Timestamp,
    /// Column `received_at` of each error row.
    pub received_at: Timestamp,
    /// `VehicleErrors.errors`, in the order of the record.
    pub errors: Vec<VehicleError>,
    /// The payload holds a field that the vendored schema does not know.
    /// See [`UnprocessedReason::UnknownFields`].
    pub unknown_fields: bool,
}

/// One error item: protobuf message `VehicleError`, one row of
/// `raw.vehicle_error`.
#[derive(Clone, PartialEq, Eq)]
pub struct VehicleError {
    /// `VehicleError.created_at`, or `None` when the item has none. Column
    /// `created_at`.
    pub created_at: Option<Timestamp>,
    /// `VehicleError.name`. Column `name`.
    pub name: String,
    /// `VehicleError.tags`. Column `tags`, a JSON object of strings.
    pub tags: BTreeMap<String, String>,
    /// `VehicleError.body`. Column `body`.
    pub body: String,
}

/// A `connectivity` record: protobuf message `VehicleConnectivity`, one row
/// of `raw.connectivity_event`.
#[derive(Clone, PartialEq, Eq)]
pub struct Connectivity {
    /// `VehicleConnectivity.vin`.
    pub vin: Vin,
    /// `VehicleConnectivity.connection_id`. Column `connection_id`.
    pub connection_id: Uuid,
    /// `VehicleConnectivity.status`: the number of the `ConnectivityEvent`
    /// enum. Column `status`.
    pub status: i32,
    /// `VehicleConnectivity.network_interface`, as sent, also when empty.
    /// Column `network_interface`.
    pub network_interface: String,
    /// `VehicleConnectivity.created_at`, the server clock. Column
    /// `created_at`.
    pub created_at: Timestamp,
    /// Column `received_at`.
    pub received_at: Timestamp,
    /// The payload holds a field that the vendored schema does not know.
    /// See [`UnprocessedReason::UnknownFields`].
    pub unknown_fields: bool,
}

// `Debug` of the types that hold vehicle values. See the module
// documentation, "Privacy".

impl fmt::Debug for SignalValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let variant = match self {
            Self::Text(_) => "Text",
            Self::Int32(_) => "Int32",
            Self::Int64(_) => "Int64",
            Self::Float32(_) => "Float32",
            Self::Float64(_) => "Float64",
            Self::Bool(_) => "Bool",
            Self::Location(_) => "Location",
            Self::Invalid(_) => "Invalid",
            Self::Enum(value) => return f.debug_tuple("Enum").field(value).finish(),
            Self::Doors(_) => "Doors",
            Self::Time(_) => "Time",
            Self::TireLocation(_) => "TireLocation",
        };
        f.debug_tuple(variant).field(&Redacted).finish()
    }
}

impl fmt::Debug for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Location")
            .field("latitude", &Redacted)
            .field("longitude", &Redacted)
            .finish()
    }
}

impl fmt::Debug for EnumValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnumValue")
            .field("value_type", &self.value_type)
            .field("number", &Redacted)
            .finish()
    }
}

impl fmt::Debug for Alert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Alert")
            .field("name", &Redacted)
            .field("audiences", &self.audiences)
            .field("started_at", &self.started_at)
            .field("ended_at", &self.ended_at)
            .finish()
    }
}

impl fmt::Debug for VehicleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VehicleError")
            .field("created_at", &self.created_at)
            .field("name", &Redacted)
            .field("tags", &Redacted)
            .field("body", &Redacted)
            .finish()
    }
}

impl fmt::Debug for Connectivity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connectivity")
            .field("vin", &self.vin)
            .field("connection_id", &self.connection_id)
            .field("status", &self.status)
            .field("network_interface", &Redacted)
            .field("created_at", &self.created_at)
            .field("received_at", &self.received_at)
            .field("unknown_fields", &self.unknown_fields)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_are_the_namespace_prefix_and_the_upstream_names() {
        assert_eq!(TOPIC_PREFIX, "tesla_telemetry_");
        for (topic, record_type) in [
            (&b"tesla_telemetry_V"[..], RecordType::VehicleData),
            (b"tesla_telemetry_alerts", RecordType::Alerts),
            (b"tesla_telemetry_errors", RecordType::Errors),
            (b"tesla_telemetry_connectivity", RecordType::Connectivity),
        ] {
            assert_eq!(RecordType::from_topic(topic), Some(record_type));
        }
    }

    #[test]
    fn a_topic_must_match_exactly() {
        for topic in [
            &b""[..],
            b"V",
            b"tesla_telemetry_",
            b"tesla_telemetry_v",
            b"tesla_telemetry_metrics",
            b"tesla_telemetry_V ",
            b" tesla_telemetry_V",
            b"tesla_telemetry_V\0",
            b"tesla_telemetry_alerts_V",
            b"other_namespace_V",
            b"TESLA_TELEMETRY_V",
            b"\xff\xfe_V",
        ] {
            assert_eq!(RecordType::from_topic(topic), None, "topic {topic:?}");
        }
    }

    #[test]
    fn reason_texts_match_adr_0002() {
        assert_eq!(UnprocessedReason::DecodeFailed.as_str(), "decode_failed");
        assert_eq!(UnprocessedReason::UnknownTopic.as_str(), "unknown_topic");
        assert_eq!(
            UnprocessedReason::MissingCreatedAt.as_str(),
            "missing_created_at"
        );
        assert_eq!(
            UnprocessedReason::UnknownValueVariant.as_str(),
            "unknown_value_variant"
        );
    }

    fn vehicle_data(unknown_value_variants: usize) -> Decoded {
        Decoded::VehicleData(VehicleData {
            vin: Vin::new("TESTVIN0000000001".to_owned()),
            created_at: Timestamp::UNIX_EPOCH,
            received_at: Timestamp::UNIX_EPOCH,
            is_resend: false,
            signals: vec![Signal {
                field: 8,
                value: SignalValue::Int32(1),
            }],
            unknown_value_variants,
            unknown_fields: false,
        })
    }

    #[test]
    fn unprocessed_reason_covers_partial_and_whole_records() {
        assert_eq!(vehicle_data(0).unprocessed_reason(), None);
        assert_eq!(
            vehicle_data(1).unprocessed_reason(),
            Some(UnprocessedReason::UnknownValueVariant)
        );
        let unprocessed = Decoded::Unprocessed(Unprocessed {
            reason: UnprocessedReason::MissingCreatedAt,
            received_at: Timestamp::UNIX_EPOCH,
        });
        assert_eq!(
            unprocessed.unprocessed_reason(),
            Some(UnprocessedReason::MissingCreatedAt)
        );
    }

    #[test]
    fn vin_debug_output_is_redacted() {
        let vin = Vin::new("TESTVIN0000000001".to_owned());
        assert_eq!(format!("{vin:?}"), "Vin([redacted])");
        assert_eq!(vin.expose(), "TESTVIN0000000001");
        let text = format!("{:?}", vehicle_data(0));
        assert!(!text.contains("TESTVIN"), "{text}");
    }

    #[test]
    fn debug_output_shows_the_variant_but_not_the_value() {
        let mut object = JsonObject::new();
        object.insert("DriverFront".to_owned(), serde_json::Value::Bool(true));
        for (value, expected) in [
            (SignalValue::Text("x".to_owned()), "Text([redacted])"),
            (SignalValue::Int32(4242), "Int32([redacted])"),
            (SignalValue::Int64(4242), "Int64([redacted])"),
            (SignalValue::Float32(42.5), "Float32([redacted])"),
            (SignalValue::Float64(42.5), "Float64([redacted])"),
            (SignalValue::Bool(true), "Bool([redacted])"),
            (
                SignalValue::Location(Location {
                    latitude: 12.5,
                    longitude: 45.25,
                }),
                "Location([redacted])",
            ),
            (SignalValue::Invalid(true), "Invalid([redacted])"),
            (
                SignalValue::Enum(EnumValue::new(60, 4242)),
                "Enum(EnumValue { value_type: 60, number: [redacted] })",
            ),
            (SignalValue::Doors(object.clone()), "Doors([redacted])"),
            (SignalValue::Time(object.clone()), "Time([redacted])"),
            (
                SignalValue::TireLocation(object),
                "TireLocation([redacted])",
            ),
        ] {
            assert_eq!(format!("{value:?}"), expected);
        }
        let location = Location {
            latitude: 12.5,
            longitude: 45.25,
        };
        assert_eq!(
            format!("{location:?}"),
            "Location { latitude: [redacted], longitude: [redacted] }"
        );
    }

    #[test]
    fn value_type_follows_the_variant() {
        let object = JsonObject::new();
        for (value, expected) in [
            (SignalValue::Text(String::new()), 1),
            (SignalValue::Int32(0), 2),
            (SignalValue::Int64(0), 3),
            (SignalValue::Float32(0.0), 4),
            (SignalValue::Float64(0.0), 5),
            (SignalValue::Bool(false), 6),
            (
                SignalValue::Location(Location {
                    latitude: 0.0,
                    longitude: 0.0,
                }),
                7,
            ),
            (SignalValue::Invalid(true), 10),
            (SignalValue::Enum(EnumValue::new(60, 4)), 60),
            (SignalValue::Doors(object.clone()), 21),
            (SignalValue::Time(object.clone()), 31),
            (SignalValue::TireLocation(object), 39),
        ] {
            assert_eq!(value.value_type(), expected, "{value:?}");
        }
    }
}
